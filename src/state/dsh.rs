//! Official DeepSeek Harness backend. Native host lifetime is independent of
//! Zerus, tmux, and individual sessions. HGS stores bindings, not credentials.
use super::*;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
// The native host's socket and start (see `rpc` and `ensure_host`).
#[cfg(unix)]
use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader},
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

#[cfg(unix)]
mod host_identity;
#[cfg(unix)]
mod shell_identity;
#[cfg(all(test, unix))]
pub(super) use host_identity::record_running_for_test;

#[cfg(unix)]
// Resident bridge identity is independent of the installed harness release.
const ADAPTER_VERSION: &str = "0.2.1-rc.2";
#[cfg(unix)]
const REVIEWED_VERSIONS: &[&str] = &["0.2.1-rc.2", "0.2.1-rc.5", "0.2.2-rc.1"];
#[cfg(unix)]
fn reviewed_harness_version(version: &str) -> bool {
    REVIEWED_VERSIONS.contains(&version)
}
#[cfg(unix)]
fn compatible_adapter(ping: &Value) -> bool {
    ping["version"] == ADAPTER_VERSION && ping["protocol"] == 1
}
#[cfg(unix)]
const BRIDGE: &str = include_str!("../dsh/bridge.mjs");
fn binding_path(name: &str) -> PathBuf {
    PROVIDER.label_path(name)
}
const PROVIDER: provider_binding::Provider = provider_binding::Provider::DeepSeek;
pub(crate) fn exists(name: &str) -> bool { PROVIDER.exists(name) }
pub(super) fn binding(name: &str) -> Result<Value> { PROVIDER.read(name) }
pub(super) fn read_id(id: &str) -> Result<Value> { PROVIDER.read_id(id) }
/// Saves the binding, then the identity its hosted tool calls carry
/// (`shell_identity`). Every path that starts a run ends here, so the file
/// names the run this save established. A file that cannot be written
/// leaves that session's tool calls without session markers, never with
/// another session's, and is reported without undoing the saved binding.
pub(super) fn save(record: &mut Value) -> Result<()> {
    PROVIDER.save(record)?;
    #[cfg(unix)]
    if let Err(error) = shell_identity::write(record) {
        eprintln!("hgs: {}'s tool calls carry no session markers until its identity can be written: {error}", string(record, "name"));
    }
    Ok(())
}
/// Forgets the binding and the identity its tool calls carried.
fn remove(record: &Value) -> Result<()> {
    PROVIDER.remove(record)?;
    #[cfg(unix)]
    shell_identity::remove(record);
    Ok(())
}
fn native_home() -> PathBuf {
    nonempty_env("DSH_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".dsh"))
}
pub(crate) fn account_context(name: &str) -> Result<Value> {
    let record = binding(name)?;
    Ok(
        json!({"provider":"dsh","id":"native-dsh","home":record["agent_home"],"run_id":record["run_id"]}),
    )
}
pub(crate) fn account_ui_url(home: &Path) -> Result<String> {
    // Reading Accounts must not start or restart a native agent host.
    let record = json!({"agent_home":home});
    let log = fs::read_to_string(host_dir(&record).join("host.log"))
        .map_err(|_| "Open the native DeepSeek UI to connect this account.".to_owned())?;
    regex::Regex::new(r"http://127\.0\.0\.1:[0-9]+/\?token=[A-Za-z0-9_%.-]+")
        .unwrap()
        .find_iter(&log)
        .last()
        .map(|m| m.as_str().to_owned())
        .ok_or_else(|| "Native DeepSeek UI is unavailable.".into())
}
fn host_dir(record: &Value) -> PathBuf {
    root().join("dsh/hosts").join(
        &format!(
            "{:x}",
            Sha256::digest(string(record, "agent_home").as_bytes())
        )[..16],
    )
}
/// The native host listens on a Unix socket; DeepSeek is a Phase 2 non-goal on
/// Windows, so nothing there starts or reaches a host.
#[cfg(not(unix))]
const UNSUPPORTED: &str =
    "DeepSeek sessions are not supported on Windows yet; run them on a Linux or macOS machine.";
#[cfg(not(unix))]
fn rpc(_: &Value, _: &str, _: Value) -> Result<Value> {
    Err(UNSUPPORTED.into())
}
#[cfg(not(unix))]
fn search_rpc(_: &Value, _: Value) -> Result<Value> {
    Err(UNSUPPORTED.into())
}
#[cfg(not(unix))]
fn ensure_host(_: &Value) -> Result<()> {
    Err(UNSUPPORTED.into())
}
const HOST_UNAVAILABLE: &str = "DeepSeek host is unavailable; resume the session to reconnect";
/// A request the host never got whole. Every `bridge.mjs` since the first
/// runs a request only once a chunk holds its terminating newline and has no
/// end handler, so a request cut short never runs: a definite refusal
/// (zerus-1wchz, pinned by `the_bridge_runs_only_a_newline_terminated_request`).
const INCOMPLETE: &str = "The DeepSeek host never received a complete request";
#[cfg(unix)]
fn rpc(record: &Value, method: &str, params: Value) -> Result<Value> {
    rpc_within(record, method, params, Duration::from_secs(35))
}
/// Interactive search waits at most this long for one host: the adapter's own
/// 5 s scan budget plus a margin. A host that does not answer counts as
/// unavailable instead of holding up every other provider's results.
#[cfg(unix)]
fn search_rpc(record: &Value, params: Value) -> Result<Value> {
    rpc_within(record, "search-history", params, Duration::from_secs(7))
}
#[cfg(unix)]
fn rpc_within(record: &Value, method: &str, params: Value, read_timeout: Duration) -> Result<Value> {
    rpc_at(
        &host_dir(record).join("host.sock"),
        method,
        params,
        read_timeout,
    )
}
#[cfg(unix)]
fn rpc_at(path: &Path, method: &str, params: Value, read_timeout: Duration) -> Result<Value> {
    let socket = crate::platform::connect_unix(path, read_timeout).map_err(|_| HOST_UNAVAILABLE)?;
    exchange(socket, method, params, read_timeout)
}
/// One request over a connected host socket.
#[cfg(unix)]
fn exchange(
    mut socket: UnixStream,
    method: &str,
    params: Value,
    read_timeout: Duration,
) -> Result<Value> {
    // The request's last byte is its newline, and the compact JSON holds no
    // other, so a failed write never wrote it and the host never ran it.
    (|| -> io::Result<()> {
        socket.set_read_timeout(Some(read_timeout))?;
        socket.set_write_timeout(Some(Duration::from_secs(10)))?;
        writeln!(socket, "{}", json!({"method":method,"params":params}))
    })()
    .map_err(|e| format!("{INCOMPLETE}: {e}"))?;
    let mut response = String::new();
    BufReader::new(socket.take(32 * 1024 * 1024))
        .read_line(&mut response)
        .map_err(|e| format!("Native request delivery uncertain: {e}"))?;
    let response: Value = serde_json::from_str(&response)
        .map_err(|e| format!("Invalid native response; delivery uncertain: {e}"))?;
    if response["ok"] != true {
        return Err(string(&response, "error").into());
    }
    Ok(response["value"].clone())
}
/// A refused connect is ECONNREFUSED, or ECONNRESET on some BSD paths: the
/// host's socket is there but the connect was not queued (zerus-xt6re).
#[cfg(unix)]
fn refused(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset
    )
}
/// The claim's connect, with a test-only refusal so the BSD full-queue answer
/// (ECONNREFUSED at once) can be exercised on Linux (zerus-xt6re).
#[cfg(unix)]
fn claim_connect(path: &Path, limit: Duration) -> io::Result<UnixStream> {
    #[cfg(test)]
    if let Some(kind) = injected_refusal() {
        return Err(io::Error::new(kind, "injected connect refusal"));
    }
    crate::platform::connect_unix(path, limit)
}
#[cfg(all(test, unix))]
thread_local! {
    static INJECTED_REFUSAL: std::cell::Cell<Option<io::ErrorKind>> =
        const { std::cell::Cell::new(None) };
}
#[cfg(all(test, unix))]
fn injected_refusal() -> Option<io::ErrorKind> {
    INJECTED_REFUSAL.with(std::cell::Cell::get)
}
#[cfg(all(test, unix))]
pub(super) fn refuse_connect_for_test(kind: io::ErrorKind) {
    INJECTED_REFUSAL.with(|refused| refused.set(Some(kind)));
}
#[cfg(all(test, unix))]
pub(super) fn allow_connect_for_test() {
    INJECTED_REFUSAL.with(|refused| refused.set(None));
}
/// A socket path exists and its verified recorded owner process is alive:
/// a refused connect to it means the host is saturated, not gone.
#[cfg(unix)]
fn live_owner_socket(dir: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    fs::symlink_metadata(dir.join("host.sock")).is_ok_and(|meta| meta.file_type().is_socket())
        && host_identity::owner_alive(dir)
}
#[cfg(unix)]
fn ensure_host(record: &Value) -> Result<()> {
    let dir = host_dir(record);
    private_dir(&dir)?;
    let _guard = lock(Some(&dir.join("start.lock")))?;
    if let Ok(ping) = rpc(record, "ping", json!({})) {
        if compatible_adapter(&ping) {
            return Ok(());
        }
        return Err("The running DeepSeek host uses an incompatible adapter; close its sessions before upgrading".into());
    }
    match UnixStream::connect(dir.join("host.sock")) {
        Ok(_) => return Err("The running DeepSeek host is not responding; it has been left running. Retry when it recovers".into()),
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => {},
        Err(e) => return Err(format!("Cannot check the native host: {e}")),
    }
    host_identity::before_start(&dir)?;
    let recommended = REVIEWED_VERSIONS.last().expect("reviewed harness version");
    let version = Command::new("dsh").arg("--version").output().map_err(|_| {
        format!("Install the official harness: npm install -g @deepseek-ai/dsh@{recommended}")
    })?;
    if !version.status.success()
        || !reviewed_harness_version(String::from_utf8_lossy(&version.stdout).trim())
    {
        return Err(format!(
            "This adapter requires official @deepseek-ai/dsh@{recommended}; installed version differs"
        ));
    }
    let script = dir.join("hgs-bridge.mjs");
    atomic(&script, BRIDGE)?;
    let socket = dir.join("host.sock");
    // The startup lock excludes competing HGS starters. Only unlink a stale
    // endpoint after a failed connection, never an active native host.
    if socket.exists() {
        fs::remove_file(&socket).map_err(|e| e.to_string())?;
    }
    let overlay = dir.join("hgs.patch.json");
    // The bridge reads each hosted session's own markers from here, one
    // file per native conversation that `save` writes (zerus-ppd26).
    let identities = shell_identity::folder(record);
    private_dir(&identities)?;
    atomic(
        &overlay,
        &json!([{"insert":[{"id":"hgs-native-dsh","name":script,"config":{"socket":socket,
            "recoveryPolicy":recovery::policy_path(),"shellIdentities":identities}}]}])
            .to_string(),
    )?;
    let log = crate::platform::private_open(OpenOptions::new().create(true).truncate(true).write(true))
        .open(dir.join("host.log"))
        .map_err(|e| e.to_string())?;
    let mut command = host_command(record, &overlay);
    command
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log);
    // Detach only the dedicated native host. Closing its launching terminal or
    // HGS process must not stop conversations.
    let mut child = host_identity::spawn(&dir, &mut command)?;
    let deadline = Instant::now() + Duration::from_secs(35);
    while Instant::now() < deadline {
        if let Ok(value) = rpc(record, "ping", json!({})) {
            if value["protocol"] == 1 {
                return Ok(());
            }
        }
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            return Err(format!(
                "Official DeepSeek host failed to start. Private log: {}",
                dir.join("host.log").display()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("DeepSeek host is still starting; retry opening the session".into())
}

/// The variables a native host never takes from the process that starts it:
/// those of the session that process runs in (`host::SESSION_CONTEXT`, which
/// the Windows session host also drops) and its terminal's name. One host
/// serves every DeepSeek session of its agent home, so whichever session
/// happened to start it is not the session of any hosted agent; each hosted
/// tool call carries its own session's markers instead (`shell_identity`).
#[cfg(unix)]
fn host_command(record: &Value, overlay: &Path) -> Command {
    let mut command = Command::new("dsh");
    #[cfg(target_os = "linux")]
    {
        let runtime = PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }));
        if runtime.join("systemd/private").exists() && crate::platform::available("systemd-run") {
            // setsid detaches the terminal, but systemd still kills every
            // descendant in the GUI's cgroup on restart. A sibling scope
            // preserves the caller's environment and private log descriptors,
            // while giving the native host an independent lifetime.
            command = Command::new("systemd-run");
            command
                .args([
                    "--user",
                    "--scope",
                    "--quiet",
                    "--collect",
                    "--expand-environment=no",
                    "--unit",
                ])
                .arg(format!("hgs-dsh-{}", uuid::Uuid::new_v4()))
                .args(["--", "dsh"])
                .env("XDG_RUNTIME_DIR", runtime);
        }
    }
    command.args(["--profile", "hgs"]);
    // This flag creates a profile and intentionally rejects existing ones.
    // Reopening the host must retain the native profile and its settings.
    if !Path::new(string(record, "agent_home"))
        .join("profiles/hgs/package.json")
        .exists()
    {
        command.args(["--from-default-profile", "web"]);
    }
    command
        .arg("--patch")
        .arg(overlay)
        .args(["--no-open", "--host", "127.0.0.1", "--port", "0"])
        .env("DSH_HOME", string(record, "agent_home"))
        .current_dir(home());
    for name in crate::state::backend::host::SESSION_CONTEXT
        .iter()
        .chain(&["HGS_TERMINAL_NAME"])
    {
        command.env_remove(name);
    }
    command
}

pub(crate) fn native_ui_url(name: &str) -> Result<String> {
    let record = if name == "@account" {
        json!({"agent_home":native_home()})
    } else {
        binding(name)?
    };
    ensure_host(&record)?;
    let pattern =
        regex::Regex::new(r"http://127\.0\.0\.1:[0-9]+/\?token=[A-Za-z0-9_%.-]+").unwrap();
    // A new host's adapter can answer before its web server logs the address.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let url = loop {
        let log = fs::read_to_string(host_dir(&record).join("host.log")).map_err(|e| e.to_string())?;
        if let Some(found) = pattern.find(&log) {
            break found.as_str().to_owned();
        }
        if std::time::Instant::now() >= deadline {
            return Err("Native UI is still starting; try again in a moment".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    if name != "@account" {
        match rpc(
            &record,
            "workspace",
            json!({"sessionId":record["conversation_id"]}),
        ) {
            Ok(_) => {}
            Err(error) if error == "Unsupported HGS native request" => {
                // Upgrade the UI of an active legacy host without restarting
                // its agents. Tokens never appear in process arguments.
                let mut child = crate::platform::spawn(
                    Command::new("node")
                        .args([
                            "--input-type=module",
                            "-e",
                            include_str!("../dsh/ui-compat.mjs"),
                        ])
                        .stdin(Stdio::piped())
                        .stdout(Stdio::null())
                        .stderr(Stdio::piped()),
                )
                .map_err(|e| e.to_string())?;
                child.stdin.take().ok_or("Could not prepare native UI")?
                    .write_all(json!({"url":url,"sessionId":record["conversation_id"],"cwd":record["cwd"]}).to_string().as_bytes())
                    .map_err(|e| e.to_string())?;
                if !child
                    .wait_with_output()
                    .map_err(|e| e.to_string())?
                    .status
                    .success()
                {
                    return Err(
                        "Could not register the session in the native DeepSeek workspace".into(),
                    );
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(url)
}

struct PermissionPlan {
    mode: String,
    legacy_url: Option<String>,
    needs_setup: bool,
    generation: Value,
}

fn legacy_permissions(record: &Value, url: &str, operation: &str) -> Result<Value> {
    let mut child = crate::platform::spawn(
        Command::new("node")
            .args([
                "--input-type=module",
                "-e",
                concat!(
                    include_str!("../dsh/account-client.mjs"),
                    "\n",
                    include_str!("../dsh/permissions-compat.mjs")
                ),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .map_err(|e| e.to_string())?;
    child.stdin.take().ok_or("Could not prepare native permissions")?
        .write_all(json!({"url":url,"sessionId":record["conversation_id"],"operation":operation,
            "pending":record["permissions_pending"] == true}).to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    let result: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    if !output.status.success() || result["ok"] != true {
        return Err(if operation == "check" {
            "Could not verify bypass permissions on the running DeepSeek host. Check its Native UI; no new session was created."
        } else {
            "DeepSeek did not confirm bypass permissions. Resume this session to finish setup; no message was sent."
        }.into());
    }
    Ok(result)
}

// Extend legacy residents through their authenticated native API, without
// reloading their bridge (which would dispose its owned live agents).
fn permission_plan(record: &Value) -> Result<PermissionPlan> {
    let config = crate::config::Config::load().map_err(|e|e.message)?;
    let mode = crate::accounts::permission_mode(&config,"native-dsh","dsh").map_err(|e|e.message)?;
    let ping = rpc(record, "ping", json!({}))?;
    let mut plan = PermissionPlan { mode, legacy_url: None, needs_setup: false, generation: ping["generation"].clone() };
    if plan.mode == "bypass" && (ping["accountPermissions"] != true || record["permissions_pending"] == true) {
        let url = account_ui_url(Path::new(string(record, "agent_home")))?;
        let checked = legacy_permissions(record, &url, "check")?;
        plan.needs_setup = checked["needs_setup"].as_bool().ok_or("Invalid native permissions response")?;
        plan.legacy_url = Some(url);
    }
    Ok(plan)
}

/// Returns the host generation the session was created or resumed under and
/// the account permission mode it used.
fn create_session(record: &mut Value) -> Result<(Value, String)> {
    // Deterministic capability/account checks precede persistence; identities
    // still precede native creation so an uncertain reply cannot duplicate it.
    let plan = permission_plan(record)?;
    if rpc(record, "ping", json!({}))?["generation"] != plan.generation {
        return Err("DeepSeek host changed before session creation; retry opening the session".into());
    }
    if plan.needs_setup { record["permissions_pending"] = json!(true); }
    save(record)?;
    rpc(record, "create", json!({"sessionId":record["conversation_id"],"cwd":record["cwd"],
        "title":string(record,"name").rsplit('/').next(),"permissionMode":plan.mode}))?;
    if let Some(url) = plan.legacy_url.filter(|_| plan.needs_setup) {
        if rpc(record, "ping", json!({}))?["generation"] != plan.generation {
            return Err("DeepSeek host changed before permission setup; resume the session".into());
        }
        legacy_permissions(record, &url, "apply")?;
        if rpc(record, "ping", json!({}))?["generation"] != plan.generation {
            return Err("DeepSeek host changed during permission setup; resume the session".into());
        }
    }
    record.as_object_mut().ok_or("Invalid DeepSeek binding")?.remove("permissions_pending");
    save(record)?;
    Ok((plan.generation, plan.mode))
}

/// Whether an RPC error leaves the request possibly executed by the host.
fn delivery_uncertain(error: &str) -> bool {
    error.contains("delivery uncertain")
}

/// Account launch defaults (zerus-v7tqa) reach only the session this launch
/// just created, through the host's existing scoped `settings` method and
/// before any prompt, so a resident older bridge needs no reload. The binding
/// records `pending` first, then `applied`, `failed` (never executed) or
/// `uncertain` (may have executed). The launch still succeeds and nothing is
/// retried; the returned text is the warning to report.
fn apply_account_defaults_with(
    record: &mut Value,
    (generation, mode): (&Value, &str),
    model: &str,
    effort: &str,
    mut call: impl FnMut(&str, Value) -> Result<Value>,
    mut persist: impl FnMut(&mut Value) -> Result<()>,
) -> Option<String> {
    let mut outcome = json!({"model":model,"effort":effort,"status":"pending","at":now()});
    record["account_defaults"] = outcome.clone();
    if let Err(error) = persist(record) {
        record["account_defaults"]["status"] = json!("failed");
        record["account_defaults"]["detail"] = json!(error);
        return Some(format!("DeepSeek session started with its native model; account defaults were not applied: {error}"));
    }
    let attempt = (|| -> std::result::Result<Value, (bool, String)> {
        let (provider, id) = model
            .split_once('/')
            .ok_or((false, "Invalid account model default".to_owned()))?;
        let ping = call("ping", json!({})).map_err(|e| (false, e))?;
        if ping["generation"] != *generation {
            return Err((false, "DeepSeek host changed after creating the session".into()));
        }
        // The mode matters only if the session was paused natively meanwhile:
        // the method then resumes it, as a scoped resume would.
        let mut params = json!({"sessionId":record["conversation_id"],"provider":provider,
            "model":id,"generation":generation,"permissionMode":mode});
        if !effort.is_empty() {
            params["reasoningEffort"] = json!(effort);
        }
        let applied = call("settings", params).map_err(|e| (delivery_uncertain(&e), e))?;
        if applied["scope"] != "session" {
            return Err((true, "DeepSeek did not confirm the session selection".into()));
        }
        Ok(applied["selected"].clone())
    })();
    let warning = match attempt {
        Ok(selected) => {
            outcome["status"] = json!("applied");
            outcome["selected"] = selected;
            None
        }
        Err((uncertain, detail)) => {
            outcome["status"] = json!(if uncertain { "uncertain" } else { "failed" });
            outcome["detail"] = json!(detail);
            Some(if uncertain {
                format!("DeepSeek may have applied the account defaults; check this session's model and effort: {detail}")
            } else {
                format!("DeepSeek session started with its native model; account defaults were not applied: {detail}")
            })
        }
    };
    record["account_defaults"] = outcome;
    match persist(record) {
        Ok(()) => warning,
        Err(error) => Some(warning.map_or_else(
            || format!("DeepSeek applied the account defaults, but recording that failed: {error}"),
            |w| format!("{w}; recording the outcome also failed: {error}"),
        )),
    }
}

pub(crate) fn launch(
    name: &str,
    cwd: &str,
    launch_id: Option<&str>,
    dry: bool,
    new_only: bool,
) -> Result<()> {
    if dry {
        println!("Create official dsh session {name} in {cwd}");
        return Ok(());
    }
    let parent = parent_link::launcher_link()?;
    let _guard = lock(Some(&root().join("dsh/bindings.lock")))?;
    if let Some(mut record) = PROVIDER.existing_for_launch(name)? {
        if parent_link::claimed_launch() { return Err("--parent applies only to new sessions; use hgs parent adopt for an existing session".into()); }
        if new_only {
            return Err("This DeepSeek session name already exists".into());
        }
        ensure_host(&record)?;
        create_session(&mut record)?;
        record["paused"] = json!(false);
        record["run_id"] = json!(uuid::Uuid::new_v4().to_string());
        save(&mut record)?;
        return Ok(());
    }
    let cwd = crate::platform::canonical(cwd).map_err(|e| e.to_string())?;
    // Only a session created here takes the account defaults; relaunching an
    // existing name above resumes it with its own stored selection.
    let config = crate::config::Config::load().map_err(|e| e.message)?;
    let defaults = crate::accounts::launch_selection(&config, "dsh").map_err(|e| e.message)?;
    let mut record = json!({"backend":"dsh","name":name,"agent":"dsh","agent_home":native_home(),"cwd":cwd,
        "conversation_id":format!("session-{}",uuid::Uuid::new_v4()),"run_id":uuid::Uuid::new_v4().to_string(),
        "created":now(),"launch_id":launch_id,"paused":false});
    if let Some(parent) = parent { record["parent"] = parent; }
    ensure_host(&record)?;
    let (generation, mode) = create_session(&mut record)?;
    if let Some((model, effort)) = defaults {
        let host = record.clone();
        if let Some(warning) = apply_account_defaults_with(
            &mut record,
            (&generation, &mode),
            &model,
            &effort,
            |method, params| rpc(&host, method, params),
            save,
        ) {
            eprintln!("hgs: {warning}");
        }
    }
    // A child its proven parent launched starts in the parent's project,
    // written after the bindings lock is released, like a fork (zerus-d5twt).
    if let Some(parent) = record
        .get("parent")
        .filter(|link| crate::swarm::placing_parent(link))
    {
        drop(_guard);
        if let Ok(config) = crate::config::Config::load() {
            crate::swarm::place_child_session(
                &config,
                name,
                &cwd,
                string(&record, "run_id"),
                &record["conversation_id"],
                parent,
            );
        }
    } else if let Ok(config) = crate::config::Config::load() {
        crate::swarm::infer_new_session(&config, name, &cwd);
    }
    Ok(())
}

pub(crate) fn snapshots() -> Result<Vec<Value>> {
    let mut out = Vec::new();
    let preferences = notifications::load();
    let mut hosts = BTreeMap::new();
    for record in binding_records() {
        let key = string(&record, "agent_home").to_owned();
        let list = hosts
            .entry(key)
            .or_insert_with(|| rpc(&record, "list", json!({})).unwrap_or(Value::Null));
        let mut value = snapshot_from(&record, list);
        notifications::enrich(&mut value, preferences.as_ref(), journal::journal_name(&record));
        out.push(value);
    }
    Ok(out)
}

/// The same listing projection, without opening every saved DeepSeek binding.
/// The host's existing list RPC and its generation semantics are unchanged.
pub(super) fn snapshot(name: &str) -> Result<Value> {
    let record = binding(name)?;
    snapshot_record(&record)
}
fn snapshot_record(record: &Value) -> Result<Value> {
    let list = rpc(record, "list", json!({})).unwrap_or(Value::Null);
    Ok(snapshot_from(record, &list))
}
/// Whether a claimed parent runs now, asking the host for at most `limit`.
/// No host is a definite no; a host that has not answered by then is busy,
/// never live.
#[cfg(unix)]
pub(super) fn claim_live(record: &Value, limit: Duration) -> Result<bool> {
    let started = std::time::Instant::now();
    let dir = host_dir(record);
    let socket = match claim_connect(&dir.join("host.sock"), limit) {
        Ok(socket) => socket,
        // BSD refuses a connect on a full listen queue at once (ECONNREFUSED,
        // sometimes ECONNRESET). The recorded owner decides: while it is alive
        // the host is saturated, not gone, so the claim is busy; a dead owner
        // or a missing socket stays a definite no. A refusal is never live
        // (zerus-xt6re).
        Err(error) if refused(&error) => {
            return if live_owner_socket(&dir) {
                Err(parent_link::CLAIM_BUSY.into())
            } else {
                Ok(false)
            };
        }
        Err(_) if started.elapsed() >= limit => return Err(parent_link::CLAIM_BUSY.into()),
        Err(_) => return Ok(false),
    };
    match exchange(socket, "list", json!({}), limit) {
        Ok(list) => {
            let row = snapshot_from(record, &list);
            Ok(row["state"] == "running" && row["process_state"] == "running")
        }
        Err(_) if started.elapsed() >= limit => Err(parent_link::CLAIM_BUSY.into()),
        Err(_) => Ok(false),
    }
}
#[cfg(not(unix))]
pub(super) fn claim_live(_: &Value, _: std::time::Duration) -> Result<bool> {
    Ok(false)
}

fn snapshot_from(record: &Value, list: &Value) -> Value {
    let row = list["items"].as_array().and_then(|items| {
        items
            .iter()
            .find(|v| v["sessionId"] == record["conversation_id"])
    });
    let row = row.unwrap_or(&Value::Null);
    let failure = idle_failure(record, row, &list["generation"]);
    let mut value = normalize(record, row, &json!({"provider_error":failure}));
    child_summaries(record, &mut value, list, &list["generation"]);
    if !list.is_null() {
        value["host_generation"] = list["generation"].clone();
    }
    value
}

// A native turn can end unsuccessfully while the agent stays alive. Cache only
// that terminal result; polling must not reload conversation pages every time.
fn idle_failure(record: &Value, row: &Value, generation: &Value) -> Value {
    if row["agentAvailable"] != true || row["running"] == true {
        return Value::Null;
    }
    let key = format!(
        "{}:{}",
        string(record, "agent_home"),
        string(record, "conversation_id")
    );
    let path = root()
        .join("dsh/errors")
        .join(format!("{:x}.json", Sha256::digest(key.as_bytes())));
    let stamp = json!([record["run_id"], row["updatedAt"], generation]);
    let cached = fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .unwrap_or(Value::Null);
    if !row["updatedAt"].is_null() && cached["stamp"] == stamp {
        return cached["error"].clone();
    }
    let Ok(inspected) = rpc(
        record,
        "inspect",
        json!({"sessionId":record["conversation_id"]}),
    ) else {
        return Value::Null;
    };
    let error = latest_failure(&inspected["page"]["records"]);
    if private_dir(path.parent().unwrap()).is_ok() {
        let _ = atomic(&path, &json!({"stamp":stamp,"error":error}).to_string());
    }
    error
}
fn turn_failure(event: &Value) -> Value {
    let reason = &event["data"]["reason"];
    let detail = match string(reason, "kind") {
        "error" => {
            let message = string(&reason["error"], "message");
            if message.is_empty() {
                "Provider request failed".to_owned()
            } else {
                message.to_owned()
            }
        }
        "max-tokens" => "Output token limit reached".to_owned(),
        _ => return Value::Null,
    };
    let mut failure = provider_errors::event(
        &format!("dsh-error-{}", event["seq"]),
        event["time"].as_f64().unwrap_or(0.0) / 1000.0,
        &detail,
        "dsh_native",
    );
    let category = provider_errors::category(&format!(
        "{} {}",
        string(reason, "kind"),
        string(&reason["error"], "code")
    ));
    if category != "provider" {
        failure["error_kind"] = json!(category);
    }
    failure
}
fn latest_failure(records: &Value) -> Value {
    for record in records.as_array().into_iter().flatten().rev() {
        let event = &record["event"];
        match string(event, "type") {
            "turn/end" => return turn_failure(event),
            "turn/start" | "user/message" => return Value::Null,
            _ => {}
        }
    }
    Value::Null
}

fn goal(value: &Value) -> Value {
    if !value.is_object() || string(value, "id").is_empty() {
        return Value::Null;
    }
    json!({"id":value["id"],"objective":value["objective"],"status":value["phase"],"revision":value["revision"],
        "rounds_used":value["roundsStarted"],"round_budget":value["maxGoalRounds"],"activation":value["activation"],
        "reason":value["blockedReason"]["message"],"source":"dsh_goal_v1"})
}

fn child_summaries(record: &Value, value: &mut Value, listed: &Value, generation: &Value) {
    if !generation.is_string() || listed["generation"] != *generation { return; }
    let Some(children) = value["subagents"].as_object_mut() else { return; };
    let mut active = 0;
    let mut completed = 0;
    let mut unknown = 0;
    for (id, child) in children.iter_mut() {
        let candidates: Vec<_> = listed["items"].as_array().into_iter().flatten()
            .filter(|row| row["sessionId"] == id.as_str() && row["parentSessionId"] == record["conversation_id"]
                && row["origin"] == "subagent").collect();
        let [row] = candidates.as_slice() else { unknown += 1; continue; };
        let (Some(running),Some(available)) = (row["running"].as_bool(),row["agentAvailable"].as_bool()) else { unknown += 1; continue; };
        if running && !available { unknown += 1; continue; }
        child["state"] = json!(if running { "working" } else { "finished" });
        child["updated"] = json!(row["updatedAt"].as_f64().unwrap_or(0.0) / 1000.0);
        child["tracking_evidence"] = json!({"source":"dsh_native_api","kind":"live_state","generation":generation,"native_id":id});
        if running { active += 1; } else { completed += 1; }
    }
    value["subagent_active_count"] = json!(active);
    value["subagent_completed_count"] = json!(completed);
    value["subagent_unknown_count"] = json!(unknown);
    value["subagent_count"] = json!(active);
    value["subagent_counts_complete"] = json!(unknown == 0);
}
fn normalize(record: &Value, row: &Value, inspected: &Value) -> Value {
    let parts = string(record, "name").splitn(3, '/').collect::<Vec<_>>();
    let projections = if inspected["baseline"].is_object() {
        &inspected["baseline"]["values"]
    } else {
        &row["projections"]["values"]
    };
    let alive = inspected["live"]
        .as_bool()
        .unwrap_or(row["agentAvailable"] == true);
    let working = row["running"] == true;
    let selection = if inspected["selection"].is_object() {
        &inspected["selection"]
    } else if row["selection"].is_object() {
        &row["selection"]
    } else if projections["modelSelection"]["next"].is_object() {
        &projections["modelSelection"]["next"]
    } else {
        &projections["modelSelection"]["lastUsed"]
    };
    let last_used = &projections["modelSelection"]["lastUsed"];
    let effort = selection
        .get("reasoningEffort")
        .filter(|v| v.is_string())
        .or_else(|| {
            (last_used["provider"] == selection["provider"]
                && last_used["model"] == selection["model"])
                .then(|| last_used.get("reasoningEffort"))
                .flatten()
        })
        .cloned()
        .unwrap_or(Value::Null);
    let pending = inspected["pendingQuestions"]
        .as_array()
        .or(row["pendingQuestions"].as_array())
        .is_some_and(|v| !v.is_empty());
    let auth_required = alive
        && inspected
            .get("authRequired")
            .unwrap_or(&row["authRequired"])
            == true;
    let phase = if pending || auth_required {
        "input"
    } else if working {
        "working"
    } else {
        "idle"
    };
    let native_goal = if inspected.get("goal").is_some() {
        &inspected["goal"]
    } else if row.get("goal").is_some() {
        &row["goal"]
    } else {
        &projections["goal"]
    };
    let outline = projections["turnOutline"].as_array().and_then(|v| v.last());
    let reply = projections["turnOutline"].as_array().and_then(|rows| {
        rows.iter()
            .rev()
            .find(|v| !string(v, "response").is_empty())
    });
    let mut children = serde_json::Map::new();
    for child in projections["subagentCatalog"]
        .as_array()
        .into_iter()
        .flatten()
    {
        children.insert(string(child,"id").to_owned(),json!({"name":child.get("label").unwrap_or(&child["id"]),"state":"unknown","mode":child["mode"]}));
    }
    let mut value = json!({"name":record["name"],"cmd":"dsh","agent":"dsh","backend":"dsh","project":parts.get(1),"tag":parts.get(2),
        "conversation_id":record["conversation_id"],"run_id":record["run_id"],"launch_id":record["launch_id"],"created":record["created"],
        "tracked":true,"resumable":true,"cwd":record["cwd"],"cwd_source":"native_session","attached":0,"clients":[],
        "state":if alive{"running"}else if record["paused"]==true{"paused"}else{"stopped"},
        "process_state":if alive{"running"}else{"exited"},"conversation_state":"active","runtime_state":if alive{"live"}else{"stopped"},
        "phase":phase,"activity":if pending || auth_required{"attention"}else if working{"busy"}else if alive{"idle"}else{"unknown"},
        "activity_summary":if auth_required{"Sign in required"}else if pending{"Needs attention"}else if working{"Working"}else if alive{"Ready"}else{"Stopped"},
        "model":selection["model"],"effort":effort,"model_provider":selection["provider"],
        "goal":goal(native_goal),"goal_source":"dsh_goal_v1","last_event_at":row["updatedAt"].as_f64().unwrap_or(0.0)/1000.0,
        "subagent_source":"dsh_native","subagent_counts_complete":false,"subagent_total_count":children.len(),"subagents":children,
        "prompt":outline.map(|v|v["prompt"].clone()),"activity_detail":outline.map(|v|v["response"].clone()),
        "reply_id":reply.map(|v|format!("dsh-turn-{}",v["seq"].as_u64().unwrap_or(0))),"reply_at":if reply.is_some(){row["updatedAt"].as_f64().unwrap_or(0.0)/1000.0}else{0.0},
        "native_ui_available":true,"settings_change_supported":alive,"send_available":alive && !auth_required && record["permissions_pending"] != true});
    if !string(record, "session_id").is_empty() {
        value["session_id"] = record["session_id"].clone();
    }
    if let Some(parent) = parent_link::listed(record) { value["parent"] = parent; }
    if pending {
        // Native question IDs distinguish one request from the next for read
        // marks and notifications; their generation-bound hashes do not.
        let ids: Vec<&str> = inspected["pendingQuestions"]
            .as_array()
            .or(row["pendingQuestions"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|q| q["id"].as_str())
            .collect();
        value["attention_id"] = json!(format!("dsh_question:{}", ids.join("\n")));
    }
    if record["permissions_pending"] == true {
        value["phase"] = json!("error");
        value["activity"] = json!("attention");
        value["activity_summary"] = json!("Resume to finish permission setup");
        value["settings_change_supported"] = json!(false);
    }
    value["account_id"] = json!("native-dsh");
    value["account_home"] = record["agent_home"].clone();
    // Always present, null for a non-fork: as fork::summary, so a client tells
    // a session that is not a fork from an older hgs without these fields.
    for key in ["fork_parent_id", "fork_source_name"] {
        value[key] = if record[key].is_string() { record[key].clone() } else { Value::Null };
    }
    // Present only while the official Schedule bundle is loaded.
    if row["schedules"].is_object() {
        value["schedules"] = json!({"count":row["schedules"]["count"],"recurring":row["schedules"]["recurring"],
            "next_at":row["schedules"]["nextAt"]});
    }
    value["auth_required"] = json!(auth_required);
    value["account_status"] = inspected
        .get("accountStatus")
        .unwrap_or(&row["accountStatus"])
        .clone();
    if alive && !working && !pending && !auth_required && record["permissions_pending"] != true {
        let failure = inspected
            .get("provider_error")
            .cloned()
            .unwrap_or_else(|| latest_failure(&inspected["page"]["records"]));
        if failure.is_object() {
            provider_errors::apply(&mut value, &failure);
        }
    }
    if let Some(todos) = projections["todos"].as_array() {
        value["task_lists"] = json!({"main":{"source":"dsh_native","run_id":record["run_id"],"items":todos.iter().enumerate().map(|(index,t)|json!({"id":index.to_string(),"title":t["content"],"status":t["status"]})).collect::<Vec<_>>()}});
    }
    let pending_recovery = inspected.get("nativeRecovery").unwrap_or(&row["nativeRecovery"]);
    if alive && !pending && !auth_required && pending_recovery.is_object() && record["permissions_pending"] != true {
        let mut error = provider_errors::event(string(pending_recovery,"id"),
            pending_recovery["at"].as_f64().unwrap_or(0.0),string(pending_recovery,"detail"),"dsh_request_error");
        error["retry_not_before"] = pending_recovery["retryNotBefore"].clone();
        provider_errors::apply(&mut value,&error);
        value["native_recovery"] = pending_recovery.clone();
    }
    recovery::enrich(record,&mut value);
    let process_source = if inspected["jobs"].is_array() || inspected["shellCalls"].is_array() { inspected } else { row };
    value["process_work"] = processes::dsh_work(record,process_source,alive && value["activity"] == "idle" && value["phase"] == "idle");
    value
}

fn native_events(records: &Value) -> Vec<Value> {
    let mut events: Vec<Value> = Vec::new();
    let mut turn_key = String::new();
    let mut last_answer: Option<usize> = None;
    for record in records.as_array().into_iter().flatten() {
        let e = &record["event"];
        let data = &e["data"];
        let kind = string(e, "type");
        if kind == "turn/start" {
            turn_key = format!("dsh-turn-{}", e["seq"].as_u64().unwrap_or(0));
            last_answer = None;
            continue;
        }
        if kind == "turn/end" && !turn_key.is_empty() {
            if let Some(index) = last_answer {
                events[index]["reply_id"] = json!(turn_key);
            }
        }
        let (typ, detail, tool) = match kind {
            "user/message"
                if ["user", "user-question-reply"].contains(&string(&data["source"], "kind")) =>
            {
                ("UserPromptSubmit", message_text(data), String::new())
            }
            "assistant/message" => (
                "AgentMessage",
                message_text(&data["message"]),
                String::new(),
            ),
            "tool/call" => (
                "PreToolUse",
                journal::clipped(&data["arguments"], 1800),
                string(data, "name").to_owned(),
            ),
            "tool/result" => (
                "PostToolUse",
                message_text(&data["message"]),
                string(&data["message"], "name").to_owned(),
            ),
            "turn/end" => {
                let failure = turn_failure(e);
                if failure.is_object() {
                    (
                        "StopFailure",
                        string(&failure, "detail").to_owned(),
                        String::new(),
                    )
                } else {
                    ("Stop", String::new(), String::new())
                }
            }
            _ => continue,
        };
        if detail.is_empty() && typ != "Stop" && typ != "PreToolUse" {
            continue;
        }
        if typ == "AgentMessage" {
            last_answer = Some(events.len());
        }
        events.push(json!({"type":typ,"agent_id":"","seq":e["seq"].as_i64().unwrap_or(0)+1,"at":e["time"].as_f64().unwrap_or(0.0)/1000.0,
            "detail":detail,"tool":tool,"source":"dsh_native","message_id":data["message"]["id"]}));
    }
    events
}
fn message_text(value: &Value) -> String {
    let content = value.get("content").unwrap_or(value);
    if let Some(s) = content.as_str() {
        return journal::clipped(&json!(s), 32000);
    }
    journal::clipped(
        &json!(content
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["type"] == "text")
            .map(|p| string(p, "text"))
            .collect::<Vec<_>>()
            .join("\n")),
        32000,
    )
}

pub(super) fn process_rpc(record: &Value, method: &str, params: Value) -> Result<Value> { rpc(record,method,params) }

/// A resident adapter advertises each native operation it implements. Older
/// adapters stay running and keep the operation in Native UI.
fn supports(ping: &Value, operation: &str) -> bool {
    ping["nativeOperations"].as_array().is_some_and(|values| values.iter().any(|value| value == operation))
}
const OLDER_ADAPTER: &str = "The running DeepSeek host uses an older adapter; use Native UI until its next ordinary start";

/// Valid DeepSeek bindings, read without contacting any host.
pub(super) fn binding_records() -> Vec<Value> { PROVIDER.records() }

fn fork_reason(record: &Value, ping: &Value, inspected: &Value) -> &'static str {
    if ping["generation"] != inspected["generation"] {
        "DeepSeek host changed; refresh before forking"
    } else if !supports(ping, "fork") || inspected["forkSupported"] != true {
        "The running DeepSeek host uses an older adapter; fork this session in Native UI"
    } else if record["permissions_pending"] == true {
        "Resume this DeepSeek session to finish permission setup before forking"
    } else if inspected["forkReady"] != true {
        "Wait for a completed reply before forking"
    } else {
        ""
    }
}

fn fork_pending_path(record: &Value, tag: Option<&str>) -> PathBuf {
    root().join("dsh/forks").join(format!("{:x}.json", Sha256::digest(format!("{}\n{}", string(record,"session_id"), json!(tag)).as_bytes())))
}

/// Old releases did not retain the requested tag or a stable source key.
/// An exact old record can be migrated by its original retry, below. If that
/// path no longer matches (rename or a newly occupied suffix), refuse rather
/// than mint a different request. This is a bounded pending-intent read only.
fn legacy_fork_unconfirmed(record: &Value) -> Result<bool> {
    let entries = match fs::read_dir(root().join("dsh/forks")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut bytes_read = 0;
    for (count, entry) in entries.enumerate() {
        if count == 5000 || std::time::Instant::now() >= until { return Err("Original fork intents cannot be checked; nothing was changed".into()); }
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_none_or(|s| s != "json") { continue; }
        let bytes = labels::read_file(&path, 16 << 10).map_err(|e| e.to_string())?.ok_or("A fork intent changed while it was read")?;
        bytes_read += bytes.len();
        if bytes_read > 4 << 20 { return Err("Original fork intents exceed the review bound; nothing was changed".into()); }
        let intent: Value = serde_json::from_slice(&bytes).map_err(|_| "An original fork intent is unreadable; check Native UI")?;
        if !intent.is_object() || !session_id::canonical(string(&intent,"request_id"))
            || string(&intent,"source_conversation_id").is_empty() || string(&intent,"generation").is_empty()
            || string(&intent,"source").is_empty() || string(&intent,"target").is_empty()
        { return Err("An original fork intent is unreadable; check Native UI".into()); }
        if intent["version"] == 2 {
            let tag = if intent["intent_tag"].is_null() { None } else { Some(intent["intent_tag"].as_str().ok_or("An original fork tag is unreadable")?) };
            if !session_id::canonical(string(&intent,"source_session_id"))
                || fork_pending_path(&json!({"session_id":intent["source_session_id"]}),tag) != path
                || !session_id::canonical(string(&intent,"request_id"))
                || string(&intent,"source_conversation_id").is_empty() || string(&intent,"generation").is_empty()
            { return Err("An original fork intent is unreadable; check Native UI".into()); }
        } else if string(&intent,"source_conversation_id") == string(record,"conversation_id") {
            return Ok(true);
        }
    }
    if std::time::Instant::now() >= until { return Err("Original fork intents cannot be checked in time".into()); }
    Ok(false)
}

/// Native fork through the resident adapter's official Session Controller.
/// The new binding keeps the source's native profile and folder and records
/// the exact identity the engine returned; the source binding never changes.
/// The engine keeps the new live agent until its host next restarts.
pub(super) fn fork(
    name: &str,
    tag: Option<&str>,
    expected_run: Option<&str>,
    expected_conversation: Option<&str>,
    dry: bool,
    scope: Option<&session_action::Scope>,
) -> Result<String> {
    let _guard = lock(Some(&root().join("dsh/bindings.lock")))?;
    let mut record = binding(name)?;
    if let Some(scope) = scope { scope.check(&record)?; }
    if expected_run.is_some_and(|id| id != string(&record, "run_id"))
        || expected_conversation.is_some_and(|id| id != string(&record, "conversation_id"))
    {
        return Err("source session changed; refresh before forking".into());
    }
    // An archived session's name is free for a new one, as for other agents.
    let mut occupied: std::collections::BTreeSet<String> = binding_records().iter()
        .chain(records()?.iter())
        .map(|record| string(record, "name").to_owned()).collect();
    occupied.insert(name.to_owned());
    let requested = fork::choose_name(name, tag, &Default::default())?;
    let mut target = fork::choose_name(name, tag, &occupied)?;
    if dry {
        println!("Fork official dsh session {name} as {target}");
        return Ok(target);
    }
    let ping = rpc(&record, "ping", json!({}))?;
    if !supports(&ping, "fork") { return Err(OLDER_ADAPTER.into()); }
    let inspected = rpc(&record, "inspect", json!({"sessionId":record["conversation_id"],"includeProcesses":false}))?;
    let reason = fork_reason(&record, &ping, &inspected);
    if !reason.is_empty() { return Err(reason.into()); }
    if string(&record, "session_id").is_empty() { save(&mut record)?; }
    if !session_id::canonical(string(&record,"session_id")) { return Err("The source session identity is unavailable; nothing was forked".into()); }
    // A fork whose reply was lost is retried under the same request ID, which
    // the same host answers with the fork it already made.
    let pending = fork_pending_path(&record, tag);
    let legacy = root().join("dsh/forks").join(format!("{:x}.json", Sha256::digest(format!("{name}\n{target}").as_bytes())));
    let previous = labels::read_file(&pending, 16 << 10).map_err(|e| format!("Cannot read the original fork intent: {e}"))?;
    let old = if previous.is_none() { labels::read_file(&legacy, 16 << 10).map_err(|e| e.to_string())? } else { None };
    if previous.is_none() && old.is_none() && legacy_fork_unconfirmed(&record)? {
        return Err("An earlier legacy fork of this conversation is unconfirmed; check Native UI before forking again".into());
    }
    let mut request_id = uuid::Uuid::new_v4().to_string();
    if let Some(bytes) = previous.as_ref().or(old.as_ref()) {
        let intent: Value = serde_json::from_slice(bytes).map_err(|_| "The original fork intent is unreadable; check Native UI")?;
        let is_legacy = previous.is_none();
        let valid_id = uuid::Uuid::parse_str(string(&intent, "request_id"))
            .is_ok_and(|id| !id.is_nil() && id.to_string() == string(&intent, "request_id"));
        if !valid_id || intent["source_conversation_id"] != record["conversation_id"]
            || intent["generation"] != ping["generation"]
            || (!is_legacy && (intent["version"] != 2 || intent["source_session_id"] != record["session_id"]
                || intent["intent_tag"] != json!(tag) || intent["cwd"] != record["cwd"] || intent["agent_home"] != record["agent_home"]))
        {
            return Err(format!("An earlier fork of {name} was not confirmed before its source or DeepSeek host changed; check Native UI before forking again"));
        }
        let original_target = string(&intent, "target");
        // The allocated target is part of the original intent. A later occupied
        // name never selects a suffix and remints the unknown native request.
        if fork::choose_name(name, original_target.rsplit('/').next(), &Default::default())? != original_target
            || occupied.contains(original_target)
        {
            return Err("The original fork destination is no longer available; check Native UI".into());
        }
        target = original_target.to_owned();
        request_id = string(&intent, "request_id").to_owned();
    }
    if !PROVIDER.vacant(&target)? || storage::label_taken(&target) {
        return Err("The original fork destination is not vacant; check Native UI".into());
    }
    if previous.is_none() {
        private_dir(pending.parent().unwrap())?;
        atomic(&pending, &json!({"version":2,"request_id":request_id,"source":name,"source_session_id":record["session_id"],
            "source_conversation_id":record["conversation_id"],"intent_tag":tag,"requested":requested,"target":target,"generation":ping["generation"],
            "cwd":record["cwd"],"agent_home":record["agent_home"],"at":now()}).to_string())?;
        if let Some(bytes) = old {
            if labels::read_file(&legacy, 16 << 10).map_err(|e| e.to_string())?.as_ref() != Some(&bytes) {
                return Err("The original fork intent changed; check Native UI".into());
            }
            archive::remove_synced(&legacy)?;
        }
    }
    if let Some(scope) = scope { scope.changing(); }
    let answer = match rpc(&record, "fork", json!({"sessionId":record["conversation_id"],"cwd":record["cwd"],
        "generation":ping["generation"],"requestId":request_id,"title":target.rsplit('/').next()})) {
        Ok(answer) => answer,
        Err(error) if error.contains("delivery uncertain") => {
            return Err(format!("{error}. Fork {name} as {target} again to adopt the native result"));
        }
        Err(error) => return Err(error),
    };
    let child = string(&answer, "sessionId");
    if answer["requestId"] != request_id.as_str() || answer["sourceSessionId"] != record["conversation_id"]
        || answer["generation"] != ping["generation"] || answer["cwd"] != record["cwd"]
        || !child.starts_with("session-") || child == string(&record, "conversation_id")
    {
        return Err("DeepSeek fork acknowledgement did not match; check Native UI".into());
    }
    let run = uuid::Uuid::new_v4().to_string();
    save(&mut json!({"backend":"dsh","name":target,"agent":"dsh","agent_home":record["agent_home"],"cwd":record["cwd"],
        "conversation_id":child,"run_id":run,"created":now(),"launch_id":null,"paused":false,
        "fork_parent_id":record["conversation_id"],"fork_source_name":name}))?;
    let _ = fs::remove_file(&pending);
    // A fork starts in its source session's project (zerus-zjsj), written
    // after the bindings lock is released (zerus-wx3k).
    drop(_guard);
    if let Ok(config) = crate::config::Config::load() {
        crate::swarm::inherit_fork_project(&config, &target, name, None, &run, &json!(child));
    }
    match scope {
        Some(scope) => scope.result(&target, &run, child, None),
        None => println!("hgs: forked {name} as {target}"),
    }
    Ok(target)
}

/// Public messages matching `query` in exact bound sessions, by binding name:
/// `Some((events, truncated))`, or `None` when that history is unavailable.
/// Searching never starts, resumes or reloads a host.
pub(super) fn history_search(records: &[&Value], query: &str, per_session: usize) -> BTreeMap<String, Option<(Vec<Value>, bool)>> {
    let mut homes: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for record in records {
        homes.entry(string(record, "agent_home")).or_default().push(record);
    }
    // Hosts are independent processes: ask them concurrently, each bounded.
    let answers: Vec<Option<Value>> = std::thread::scope(|scope| {
        let queries: Vec<_> = homes.values().map(|group| scope.spawn(move || {
            let ids: Vec<&str> = group.iter().map(|record| string(record, "conversation_id")).collect();
            search_rpc(group[0], json!({"query":query,"sessionIds":ids,"perSession":per_session})).ok()
        })).collect();
        queries.into_iter().map(|query| query.join().ok().flatten()).collect()
    });
    let mut found = BTreeMap::new();
    for (group, answer) in homes.values().zip(answers) {
        for record in group {
            // The host's time budget skips later sessions: limited, not unavailable.
            let session = answer.as_ref().and_then(|answer| answer["sessions"].as_array()?.iter()
                .find(|session| session["sessionId"] == record["conversation_id"] && ["ok", "skipped"].contains(&string(session, "status"))));
            found.insert(storage::owner_key(record), session.map(|session| {
                let rows: Vec<Value> = session["events"].as_array().into_iter().flatten().map(|event| json!({"event":event})).collect();
                (native_events(&json!(rows)), session["truncated"] == true || session["status"] == "skipped")
            }));
        }
    }
    found
}

fn schedule_item(entry: &Value) -> Value {
    let mut record = entry.clone();
    if let Some(fields) = record.as_object_mut() {
        for key in ["sessionId", "status", "lastDelivery"] { fields.remove(key); }
    }
    let next = chrono::DateTime::parse_from_rfc3339(string(entry, "scheduledAt")).map(|at| at.timestamp_millis() as f64 / 1000.0).ok();
    let recurring = ["every", "daily", "weekly", "cron", "rrule"].contains(&string(entry, "kind"));
    let delivery = entry.get("lastDelivery").cloned().unwrap_or(Value::Null);
    json!({"id":entry["id"],"title":entry["title"],"prompt":entry["prompt"],"kind":entry["kind"],"status":entry["status"],
        "recurring":recurring,"scheduled_at":next,"last_delivery":delivery,"record":record})
}

/// Official DeepSeek Schedule tasks of one exact session: list them, or with
/// `--json` delete, edit (native compare-and-set) or read delivery history.
fn schedules(name: &str, record: &Value, args: &[String]) -> Result<Value> {
    if record["permissions_pending"] == true && args.iter().any(|arg| arg == "--json") {
        return Err("Resume this DeepSeek session to finish permission setup before managing its schedules".into());
    }
    let ping = rpc(record, "ping", json!({}))?;
    if !supports(&ping, "schedules") { return Err(OLDER_ADAPTER.into()); }
    let identity = json!({"name":name,"run_id":record["run_id"],"conversation_id":record["conversation_id"]});
    let unavailable = "Native Schedule is not enabled for this DeepSeek profile; enable Automation tasks in Native UI Plugins";
    if !args.iter().any(|arg| arg == "--json") {
        let listed = rpc(record, "schedules", json!({"sessionId":record["conversation_id"],"action":"list"}))?;
        if listed["available"] != true { return Err(unavailable.into()); }
        let mut output = identity;
        output["generation"] = listed["generation"].clone();
        output["items"] = json!(listed["items"].as_array().into_iter().flatten().map(schedule_item).collect::<Vec<_>>());
        return Ok(output);
    }
    let request = checked_request(record)?;
    let action = string(&request, "action");
    if !["delete", "update", "history"].contains(&action) { return Err("Choose delete, update or history".into()); }
    if request["expected_generation"] != ping["generation"] { return Err("DeepSeek host changed; refresh schedules".into()); }
    let mut params = json!({"sessionId":record["conversation_id"],"action":action,"generation":ping["generation"],"id":request["id"]});
    for (from, to) in [("expected", "expected"), ("title", "title"), ("prompt", "prompt"), ("change", "change"), ("limit", "limit"), ("before", "before")] {
        if !request[from].is_null() { params[to] = request[from].clone(); }
    }
    let answer = rpc(record, "schedules", params)?;
    if answer["available"] != true { return Err(unavailable.into()); }
    if answer["generation"] != ping["generation"] { return Err("DeepSeek host changed during the schedule change".into()); }
    let mut output = identity;
    output["request_id"] = request["request_id"].clone();
    output["action"] = json!(action);
    output["result"] = answer["result"].clone();
    Ok(output)
}

pub(super) fn processes(name: &str, output: Option<&str>, stop: Option<&str>, run: Option<&str>, conversation: Option<&str>, generation: Option<&str>) -> Result<i32> {
    let record=binding(name)?;
    if (output.is_some() || stop.is_some()) && (run!=Some(string(&record,"run_id")) || conversation!=Some(string(&record,"conversation_id"))) {
        return Err("The process belongs to a different session run or conversation".into());
    }
    let inspected=rpc(&record,"inspect",json!({"sessionId":record["conversation_id"]}))?;
    println!("{}",processes::dsh_dispatch(&record,&inspected,output,stop,generation)?);Ok(0)
}

pub(crate) fn inspect(name: &str, agent_id: Option<&str>) -> Result<Value> {
    // Internal recovery/input checks never need a process inventory.
    inspect_with_processes(name, agent_id, false)
}
/// `hgs inspect NAME` with its process inventory, for `hgs watch`.
pub(crate) fn inspect_processes(name: &str) -> Result<Value> {
    inspect_with_processes(name, None, true)
}
fn inspect_with_processes(name: &str, agent_id: Option<&str>, include_processes: bool) -> Result<Value> {
    let record = binding(name)?;
    let inspected = rpc(
        &record,
        "inspect",
        json!({"sessionId":record["conversation_id"],"agentId":agent_id,"includeProcesses":include_processes}),
    )?;
    let listed = rpc(&record, "list", json!({}))?;
    let row = listed["items"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|v| v["sessionId"] == record["conversation_id"])
        })
        .unwrap_or(&Value::Null);
    let child_row = agent_id.and_then(|id| listed["items"].as_array().and_then(|items| {
        let rows: Vec<_> = items.iter().filter(|child| child["sessionId"] == id
            && child["parentSessionId"] == record["conversation_id"] && child["origin"] == "subagent").collect();
        if let [row] = rows.as_slice() { Some(*row) } else { None }
    })).filter(|_| listed["generation"] == inspected["generation"]);
    let mut result = normalize(&record, child_row.unwrap_or(row), &inspected);
    if agent_id.is_none() { waits::decorate(std::slice::from_mut(&mut result)); }
    child_summaries(&record, &mut result, &listed, &inspected["generation"]);
    let mut events = native_events(&inspected["page"]["records"]);
    let process_snapshot=if include_processes { processes::dsh_snapshot(&record,&inspected) } else { Value::Null };
    for event in &mut events {
        let source=inspected["page"]["records"].as_array().into_iter().flatten().map(|v|&v["event"]).find(|e|e["seq"]==event["seq"]);
        if let Some(source)=source {
            let call=source["data"].get("callId").or_else(||source["data"]["message"].get("toolCallId"));
            if let Some(job)=process_snapshot["items"].as_array().into_iter().flatten().find(|j|j["owner"]=="main" && Some(&j["call_id"])==call) {event["process_id"]=job["id"].clone();}
        }
    }
    result["turn_started"] = json!(inspected["page"]["records"].as_array().into_iter().flatten().rev()
        .find(|v|v["event"]["type"]=="turn/start").and_then(|v|v["event"]["time"].as_f64()).unwrap_or(0.0)/1000.0);
    for event in &events {
        if event["type"] == "UserPromptSubmit" {
            result["recovery_user_at"] = event["at"].clone();
            result["recovery_user_text"] = event["detail"].clone();
        }
    }
    for event in inspected["page"]["records"].as_array().into_iter().flatten().map(|v|&v["event"]) {
        if event["type"] == "turn/end" && event["data"]["reason"]["kind"] == "completed" {
            result["recovery_success_at"] = json!(event["time"].as_f64().unwrap_or(0.0) / 1000.0);
        }
        if event["type"] == "turn/end" && ["aborted","interrupted","blocked"].contains(&string(&event["data"]["reason"],"kind")) {
            result["recovery_cancel_at"] = json!(event["time"].as_f64().unwrap_or(0.0) / 1000.0);
        }
    }
    result["cursor"] = json!(0);
    result["events"] = json!(events);
    result["attachment_messages"] = attachments::messages(&record, agent_id.unwrap_or(""));
    result["voice_messages"] = conversation_history::inspected_voice_messages(&record, &result, agent_id);
    result["replace_events"] = json!(true);
    result["history_truncated"] = inspected["page"]["hasMore"].clone();
    result["host_generation"] = inspected["generation"].clone();
    let ping=rpc(&record,"ping",json!({})).unwrap_or(Value::Null);
    result["terminal_supported"]=json!(false);
    result["terminal_reason"]=json!(backend::DEEPSEEK_TERMINAL.text());
    result["allowed_actions"]=if agent_id.is_none() && ping["sessionActions"]==true && ping["generation"]==inspected["generation"] {
        inspected["lifecycleActions"].clone()
    } else {json!([])};
    result["action_reasons"]=json!({"archive":"Native DeepSeek has no archive lifecycle API","fork":"Native DeepSeek has no fork lifecycle API",
        "terminate":"Use Pause for this native DeepSeek session","restore":"Native DeepSeek has no archive lifecycle API",
        "pause":"Requires a supported resident adapter and its owned handle","resume":"Requires a supported resident adapter and a stopped session",
        "rename":"Requires the supported resident scoped adapter","forget":"Requires a supported resident adapter and its owned handle"});
    let fork_reason = if agent_id.is_some() { "Fork the parent DeepSeek session" } else { fork_reason(&record, &ping, &inspected) };
    result["fork_supported"] = json!(fork_reason.is_empty());
    result["fork_reason"] = json!(fork_reason);
    if fork_reason.is_empty() {
        if let Some(actions) = result["allowed_actions"].as_array_mut() { actions.push(json!("fork")); }
        result["action_reasons"].as_object_mut().unwrap().remove("fork");
    } else {
        result["action_reasons"]["fork"] = json!(fork_reason);
    }
    result["pending_questions"] = json!(question_cards(&record, &inspected));
    result["native_questions"] = inspected["pendingQuestions"].clone();
    result["native_projections"] = inspected["baseline"]["values"].clone();
    result["session_usage"] = usage::dsh(&inspected["baseline"]["values"]);
    if include_processes { result["processes"] = process_snapshot; }
    result["compact_context_supported"] = json!(agent_id.is_none() && inspected["compactSupported"] == true && result["phase"] == "idle");
    result["compact_context_request"] = inspected["compactRequest"].clone();
    if result["compact_context_request"].is_object() {
        result["compact_context_request"]["run_id"] = record["run_id"].clone();
        result["compact_context_request"]["conversation_id"] = record["conversation_id"].clone();
        if result["compact_context_request"]["status"] == "compacting" {
            result["phase"] = json!("compacting"); result["activity"] = json!("busy");
            result["compaction_started"] = result["compact_context_request"]["at"].clone();
            result["compact_context_supported"] = json!(false);
        }
    }
    result["interrupt_supported"] = json!(inspected["interruptSupported"] == true &&
        ["working","tool","compacting"].contains(&string(&result,"phase")) && result["process_state"] == "running" && result["turn_started"].as_f64().unwrap_or(0.0)>0.0);
    if let Some(id) = agent_id {
        // The child's native status must never inherit the parent's running flag.
        let children: Vec<_> = listed["items"].as_array().into_iter().flatten()
            .filter(|child| child["sessionId"] == id && child["parentSessionId"] == record["conversation_id"]
                && child["origin"] == "subagent").collect();
        if let [child] = children.as_slice() {
            if listed["generation"] == inspected["generation"] && child["running"].is_boolean()
                && child["agentAvailable"].is_boolean() && (child["running"] != true || child["agentAvailable"] == true) {
                let running = child["running"] == true;
                if !["input","approval","error"].contains(&string(&result,"phase")) {
                    result["phase"] = json!(if running { "working" } else { "idle" });
                    result["activity"] = json!(if running { "busy" } else { "idle" });
                    result["activity_summary"] = json!(if running { "Working" } else { "Ready" });
                }
                result["tracking_evidence"] = json!({"source":"dsh_native_api","kind":"live_state","generation":inspected["generation"],"native_id":id});
            } else {
                result["phase"] = json!("unknown");
                result["activity"] = json!("unknown");
                result["activity_summary"] = json!("Status unavailable");
            }
        } else {
            result["phase"] = json!("unknown");
            result["activity"] = json!("unknown");
            result["activity_summary"] = json!("Status unavailable");
        }
        result["agent_id"] = json!(id);
        result["parent_conversation_id"] = record["conversation_id"].clone();
        result["conversation_id"] = json!(format!("{}/{id}", string(&record, "conversation_id")));
        result["send_supported"] = inspected["childSendSupported"].clone();
        result["read_only"] = json!(inspected["childSendSupported"] != true);
        result["child_mode"] = inspected["childMode"].clone();
        result["history_scope"] = json!("Native DeepSeek subagent history");
    } else if let Ok(catalog) = rpc(&record, "catalog", json!({})) {
        if string(&result, "effort").is_empty() {
            if let Some(model) = catalog["groups"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|g| g["id"] == result["model_provider"])
                .and_then(|g| g["models"].as_array())
                .and_then(|models| models.iter().find(|m| m["id"] == result["model"]))
            {
                result["effort"] = model["reasoning"]["defaultEffort"].clone();
            }
        }
        result["settings_model"] = json!(format!(
            "{}/{}",
            string(&result, "model_provider"),
            string(&result, "model")
        ));
        result["model_options"] = json!(model_options(&catalog));
    }
    if agent_id.is_none() && record.get("account_defaults").is_some() {
        result["account_defaults"] = record["account_defaults"].clone();
    }
    if agent_id.is_none() { notifications::enrich(&mut result, notifications::load().as_ref(), journal::journal_name(&record)); }
    Ok(result)
}

/// Read-only resident host methods; never ensure_host or a lifecycle RPC.
/// Native catalog choices as `provider/model`, labelled "Model (Provider)".
fn model_options(catalog: &Value) -> Vec<Value> {
    catalog["groups"].as_array().into_iter().flatten().flat_map(|g| {
        g["models"].as_array().into_iter().flatten().map(move |m| json!({
            "id":format!("{}/{}",string(g,"id"),string(m,"id")),
            "label":format!("{} ({})",string(m,"name"),string(g,"name")),
            "effort_options":m["reasoning"]["efforts"].as_array().into_iter().flatten().map(|e|e["id"].clone()).collect::<Vec<_>>()}))
    }).collect()
}

/// Settings > Accounts: the resident host's catalog through its read-only
/// `catalog` method. Never starts, restarts or reloads a host; without one
/// the list is empty. The native default (agent-default-model) comes along.
pub(crate) fn account_models(home: &Path) -> Value {
    let catalog = rpc(&json!({"agent_home":home}), "catalog", json!({})).unwrap_or(Value::Null);
    account_models_from(&catalog)
}
fn account_models_from(catalog: &Value) -> Value {
    let models = model_options(catalog);
    let default = &catalog["default"];
    let model = match (string(default, "provider"), string(default, "model")) {
        (provider, model) if !provider.is_empty() && !model.is_empty() => format!("{provider}/{model}"),
        _ => String::new(),
    };
    json!({"models":models.iter().take(64).collect::<Vec<_>>(),"truncated":models.len()>64,
        "provider_defaults":{"model":model,"effort":string(default,"reasoningEffort")}})
}

pub(super) fn notification_snapshot(record: &Value) -> Result<(Value, Option<Vec<Value>>)> {
    notification_snapshot_with(record, |method, params| rpc(record, method, params))
}
fn notification_snapshot_with(record: &Value, mut observe: impl FnMut(&str, Value) -> Result<Value>) -> Result<(Value, Option<Vec<Value>>)> {
    let inspected = observe("inspect", json!({"sessionId":record["conversation_id"],"includeProcesses":false}))?;
    let listed = observe("list", json!({}))?;
    if !inspected["generation"].is_string() || inspected["generation"] != listed["generation"] { return Err("baseline_unavailable".into()); }
    let pending = inspected["pendingQuestions"].as_array().ok_or("baseline_unavailable")?;
    if pending.iter().any(|question| string(question, "id").is_empty() || string(question, "id").len() > 256 || string(question, "id").chars().any(char::is_control)) { return Err("baseline_unavailable".into()); }
    let row = listed["items"].as_array().and_then(|rows| rows.iter().find(|row| row["sessionId"] == record["conversation_id"])).ok_or("baseline_unavailable")?;
    let mut view = normalize(record, row, &inspected);
    child_summaries(record, &mut view, &listed, &inspected["generation"]);
    // Listing read signatures represent these requests through attention_id;
    // only Codex optional questions have a separate listing question set here.
    Ok((view, Some(Vec::new())))
}

fn question_cards(record: &Value, inspected: &Value) -> Vec<Value> {
    inspected["pendingQuestions"].as_array().into_iter().flatten().map(|q| {
        let items=q["questions"].as_array().into_iter().flatten().map(|item|json!({
            "id":item["id"],"question":item["question"],"header":item["header"],"body":item["detail"],"multi_select":item["multiSelect"],
            "allow_other":q["approval"]!=true,"required":true,"options":item["options"].as_array().into_iter().flatten().enumerate().map(|(i,o)|json!({"id":if q["approval"]==true {if i==0 {"allow".to_owned()}else{"deny".to_owned()}}else{i.to_string()},"label":o["label"],"description":o["description"]})).collect::<Vec<_>>()
        })).collect::<Vec<_>>();
        let hash=format!("{:x}",Sha256::digest(json!([inspected["generation"],q]).to_string().as_bytes()));
        json!({"question_id":q["id"],"question_hash":hash,"tool_call_id":q["id"],"run_id":record["run_id"],"conversation_id":record["conversation_id"],
            "agent_id":"main","source":"dsh_native","approval":q["approval"]==true,"can_answer":true,"answer_transport":"native_dsh","questions":items})
    }).collect()
}

pub(super) fn recover(snapshot: &Value, request_id: &str) -> Result<Value> {
    let _guard = lock(None)?;
    let record = binding(string(snapshot,"name"))?;
    if record["permissions_pending"] == true {
        return Err("Resume this DeepSeek session to finish permission setup before recovery".into());
    }
    if record["run_id"] != snapshot["run_id"] || record["conversation_id"] != snapshot["conversation_id"] {
        return Err("DeepSeek session changed before recovery".into());
    }
    recovery::check_native_submission(snapshot)?;
    let fresh=inspect(string(snapshot,"name"),None)?;
    for key in ["run_id","conversation_id","host_generation","model","provider_error","recovery_user_at","recovery_cancel_at"] {
        if fresh[key]!=snapshot[key] {return Err("DeepSeek failed request changed before recovery".into());}
    }
    if fresh["phase"]!="error" || fresh["process_state"]!="running" {
        return Err("DeepSeek is no longer waiting after this failure".into());
    }
    if snapshot["native_recovery"].is_object() {
        rpc(&record,"recover",json!({"sessionId":record["conversation_id"],"id":snapshot["native_recovery"]["id"],
            "generation":snapshot["host_generation"],"requestId":request_id}))
    } else {
        let native=rpc(&record,"send",json!({"sessionId":record["conversation_id"],"requestId":request_id,
            "content":[{"type":"text","text":recovery::CONTINUATION}]}))?;
        Ok(json!({"status":"confirmed","request_id":request_id,"native":native}))
    }
}
pub(super) fn dismiss_recovery(snapshot: &Value) -> Result<Value> {
    let record = binding(string(snapshot,"name"))?;
    rpc(&record,"cancel-recovery",json!({"sessionId":record["conversation_id"],"id":snapshot["native_recovery"]["id"]}))
}

pub(super) fn interrupt(snapshot: &Value) -> Result<Value> {
    let record=binding(string(snapshot,"name"))?;
    if record["run_id"]!=snapshot["run_id"] || record["conversation_id"]!=snapshot["conversation_id"] {
        return Err("DeepSeek session changed before interruption".into());
    }
    interrupt_with(&record, snapshot, rpc)
}
/// The interrupt request of `snapshot`'s turn, through `rpc`, with each
/// error classified by [`interrupt_refusal`].
fn interrupt_with(
    record: &Value,
    snapshot: &Value,
    rpc: impl FnOnce(&Value, &str, Value) -> Result<Value>,
) -> Result<Value> {
    rpc(record,"interrupt",json!({"sessionId":record["conversation_id"],"generation":snapshot["host_generation"],"turnStarted":snapshot["turn_started"]}))
        .map_err(interrupt_refusal)
}
/// The host disarms goals and aborts a compaction before its native cancel,
/// so only a refusal before those effects is definite (zerus-1wchz): no host
/// to reach, a request it never got whole, or one of the bridge's own checks
/// ahead of them.
fn interrupt_refusal(error: String) -> String {
    const BEFORE_EFFECTS: [&str; 4] = [
        HOST_UNAVAILABLE,
        "DeepSeek host changed",
        "Use the parent to manage a subagent",
        "DeepSeek turn changed; nothing was stopped",
    ];
    if BEFORE_EFFECTS.contains(&error.as_str())
        || error.starts_with(INCOMPLETE)
        || error.contains("delivery uncertain")
    {
        error
    } else {
        format!("Interrupt delivery uncertain: {error}")
    }
}

fn checked_request(record: &Value) -> Result<Value> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(30 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 30 * 1024 * 1024 {
        return Err("Native request is too large".into());
    }
    let request: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    uuid::Uuid::parse_str(string(&request, "request_id")).map_err(|_| "Invalid request ID")?;
    if request["expected_run_id"] != record["run_id"]
        || request["expected_conversation_id"] != record["conversation_id"]
    {
        return Err("Session changed; refresh before trying again".into());
    }
    Ok(request)
}
/// Lifecycle remains confined to a supported resident host. Never start,
/// dispose, reload or replace a host to implement a mobile scoped action.
pub(super) fn session_action(name: &str, action: &str, new_name: Option<&str>, scope: &session_action::Scope) -> Result<()> {
    if !["pause","resume","rename","forget"].contains(&action) {return Err("This native DeepSeek adapter has no archive lifecycle API".into());}
    let _guard=lock(Some(&root().join("dsh/bindings.lock")))?;
    let mut record=binding(name)?;scope.check(&record)?;
    if let Some(new)=new_name {
        let old:Vec<_>=name.split('/').collect();let parts:Vec<_>=new.split('/').collect();
        if parts.len()!=3 || old.len()!=3 || parts[..2]!=old[..2] || parts.iter().any(|p|p.is_empty() || p.trim()!=*p || p.contains([':', '.']) || p.chars().any(char::is_control)) {
            return Err("Rename must preserve the original DeepSeek provider and project".into());
        }
        if new!=name && exists(new) {return Err("The destination session already exists".into());}
    }
    if action=="rename" {
        if legacy_fork_unconfirmed(&record)? { return Err("Resolve the original legacy fork before renaming this session".into()); }
        PROVIDER.prepare_label(&record,new_name.unwrap())?;
    }
    let ping=rpc(&record,"ping",json!({}))?;
    if ping["sessionActions"]!=true {return Err("The resident DeepSeek adapter lacks scoped lifecycle; it was left running".into());}
    let inspected=rpc(&record,"inspect",json!({"sessionId":record["conversation_id"],"includeProcesses":false}))?;
    if ping["generation"]!=inspected["generation"] || !inspected["lifecycleActions"].as_array().is_some_and(|values|values.iter().any(|value|value==action)) {
        return Err("This action is unavailable for the exact native DeepSeek handle".into());
    }
    let mode=crate::accounts::permission_mode(&crate::config::Config::load().map_err(|e|e.message)?,"native-dsh","dsh").map_err(|e|e.message)?;
    scope.changing();
    let answer=rpc(&record,"session-action",json!({"sessionId":record["conversation_id"],"generation":inspected["generation"],
        "action":action,"title":new_name.map(|name|name.rsplit('/').next().unwrap()),"permissionMode":mode}))?;
    if answer["sessionId"]!=record["conversation_id"] || answer["action"]!=action {return Err("DeepSeek lifecycle acknowledgement did not match".into());}
    if answer["generation"]!=inspected["generation"] {return Err("DeepSeek host changed during lifecycle handoff".into());}
    if answer["status"]=="failed" {scope.unchanged();return Err(string(&answer,"error").into());}
    if answer["status"]!="completed" {return Err("DeepSeek lifecycle handoff was not confirmed".into());}
    match action {
        "pause"=>record["paused"]=json!(true),
        "resume"=>{record["paused"]=json!(false);record["run_id"]=json!(uuid::Uuid::new_v4().to_string());record.as_object_mut().unwrap().remove("permissions_pending");},
        "rename"=>{notifications::anchor(&mut record);if storage::in_id_layout(&record) { recovery::rename_job_in_place(&record,new_name.unwrap())?; } else { recovery::rename_job(name,new_name.unwrap(),string(&record,"run_id"))?; } record["name"]=json!(new_name.unwrap());},
        "forget"=>{remove(&record)?;return Ok(());},
        _=>unreachable!(),
    }
    save(&mut record)?;
    if action=="rename" && new_name!=Some(name) && !storage::in_id_layout(&record) {
        archive::remove_synced(&binding_path(name))?;
        // The rename is done; losing read marks only shows a reply as unread again.
        let _=read_marks::rename_owner(name,new_name.unwrap());
    }
    scope.result_record(&record);Ok(())
}

pub(crate) fn dispatch(command: &str, args: &[String]) -> Result<i32> {
    let name = args.first().ok_or("Choose a DeepSeek session")?;
    let _guard = if !["inspect", "exists", "schedules"].contains(&command) {
        Some(lock(Some(&root().join("dsh/bindings.lock")))?)
    } else {
        None
    };
    let mut record = binding(name)?;
    match command {
        "inspect" => {
            let agent = args
                .windows(2)
                .find(|p| p[0] == "--agent")
                .map(|p| p[1].as_str());
            println!("{}", inspect_with_processes(name, agent, !args.iter().any(|s|s=="--skip-processes"))?);
        }
        "compact-context" => {
            let request = checked_request(&record)?;
            let current = inspect(name, None)?;
            if current["compact_context_supported"] != true { return Err("Native compaction is unavailable or the agent is busy".into()); }
            rpc(&record, "compact", json!({"sessionId":record["conversation_id"],"generation":current["host_generation"],"requestId":request["request_id"]}))?;
            println!("{}",json!({"request_id":request["request_id"],"name":name,"run_id":record["run_id"],"conversation_id":record["conversation_id"],"status":"submitted"}));
        }
        "send" | "settings" => {
            if record["permissions_pending"] == true {
                return Err("Resume this DeepSeek session to finish permission setup before sending or changing settings".into());
            }
            let request = checked_request(&record)?;
            let receipt = json!({"request_id":request["request_id"],"name":name,"run_id":record["run_id"],"conversation_id":record["conversation_id"],"at":now()});
            let mut receipt = receipt;
            if command == "send" {
                let agent_id = string(&request, "agent_id");
                let text = string(&request, "text");
                if text.len() > 65536 {
                    return Err("Message exceeds 64 KiB".into());
                }
                let files = request["attachments"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let references = files
                    .iter()
                    .map(|f| string(f, "reference"))
                    .collect::<Vec<_>>();
                attachments::validate_references(&references)?;
                for file in &files {
                    if !["image/png", "image/jpeg", "image/webp", "image/gif"]
                        .contains(&string(file, "mime"))
                    {
                        return Err("This DeepSeek composer supports image attachments; use the native UI for other files".into());
                    }
                }
                let mut content = Vec::new();
                for part in attachments::ordered_parts(text, &references) {
                    match part {
                        attachments::MessagePart::Text(value) => {
                            if !value.trim().is_empty() {
                                content.push(json!({"type":"text","text":value}));
                            }
                        }
                        attachments::MessagePart::Attachment(index) => {
                            let file = &files[index];
                            if !references[index].is_empty() && !text.contains(references[index]) {
                                content.push(json!({"type":"text","text":references[index]}));
                            }
                            content.push(json!({"type":"image","mediaType":file["mime"],"data":file["data_base64"],"name":file["name"]}));
                        }
                    }
                }
                if content.is_empty() {
                    return Err("Message is empty".into());
                }
                let receipt_path = root()
                    .join("dsh/receipts")
                    .join(format!("{}.json", string(&request, "request_id")));
                let digest = format!(
                    "{:x}",
                    Sha256::digest(json!([name, request]).to_string().as_bytes())
                );
                if receipt_path.exists() {
                    let previous: Value = serde_json::from_slice(
                        &fs::read(&receipt_path).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?;
                    if previous["digest"] != digest {
                        return Err("This request ID was already used for another message".into());
                    }
                    if previous["status"] == "submitted" {
                        println!("{previous}");
                        return Ok(0);
                    }
                    return Err("Delivery uncertain: this message may already be in DeepSeek. Check Activity before sending it again".into());
                }
                if !agent_id.is_empty() {
                    let child = inspect(name, Some(agent_id))?;
                    if child["send_supported"] != true {
                        return Err("This child cannot accept messages; it must be continuable with a live parent".into());
                    }
                    receipt["agent_id"] = json!(agent_id);
                }
                // Authentication is a definite preflight failure, not an
                // uncertain delivery. Do not leave a pending receipt for it.
                let readiness = rpc(
                    &record,
                    "inspect",
                    json!({"sessionId":record["conversation_id"]}),
                )?;
                if readiness["authRequired"] == true {
                    return Err(
                        "Sign in to DeepSeek in the native UI before sending a message".into(),
                    );
                }
                receipt["digest"] = json!(digest);
                receipt["attachments"] = attachments::stage_native(&request)?;
                receipt["text"] = json!(text);
                receipt["submitted_text"] = json!(text);
                receipt["submitted_at"] = json!(now());
                receipt["status"] = json!("pending");
                if agent_id.is_empty() {
                    if let Some(target) = session_graph::target_receipt(&record) {
                        receipt["graph_target"] = target;
                    }
                }
                atomic(&receipt_path, &receipt.to_string())?;
                if agent_id.is_empty() {
                    rpc(
                        &record,
                        "send",
                        json!({"sessionId":record["conversation_id"],"requestId":request["request_id"],"content":content,"mode":"steer","expectedCompactionId":request["expected_compaction_id"]}),
                    )?;
                } else {
                    rpc(
                        &record,
                        "send-child",
                        json!({"parentSessionId":record["conversation_id"],"childSessionId":agent_id,
                        "requestId":request["request_id"],"content":content,"mode":"continuable","delivery":"steer"}),
                    )?;
                }
                receipt["status"] = json!("submitted");
                receipt["transport"] = json!("native_dsh");
                atomic(&receipt_path, &receipt.to_string())?;
            } else {
                let current = inspect(name, None)?;
                let selected = if string(&request, "model").is_empty() {
                    string(&current, "settings_model")
                } else {
                    string(&request, "model")
                };
                let (provider, model) = selected
                    .split_once('/')
                    .ok_or("Choose a native provider and model")?;
                // A paused session resumes under the account permission mode.
                let mode = crate::accounts::permission_mode(&crate::config::Config::load().map_err(|e|e.message)?,"native-dsh","dsh").map_err(|e|e.message)?;
                let mut params = json!({"sessionId":record["conversation_id"],"provider":provider,"model":model,"permissionMode":mode});
                if !string(&request, "effort").is_empty() {
                    params["reasoningEffort"] = request["effort"].clone();
                }
                let applied = rpc(&record, "settings", params)?;
                receipt["status"] = json!("applied");
                receipt["scope"] = json!("session");
                receipt["model"] = json!(selected);
                receipt["effort"] = applied["selected"]["reasoningEffort"].clone();
            }
            println!("{receipt}");
        }
        "answer" => {
            let request = checked_request(&record)?;
            let inspected = rpc(
                &record,
                "inspect",
                json!({"sessionId":record["conversation_id"]}),
            )?;
            let cards = question_cards(&record, &inspected);
            let card = cards
                .iter()
                .find(|q| {
                    q["question_id"] == request["question_id"]
                        && q["question_hash"] == request["expected_question_hash"]
                })
                .ok_or("This question changed or is no longer pending")?;
            let mut answers = Vec::new();
            for answer in request["answers"]
                .as_array()
                .ok_or("Answers are required")?
            {
                let item = card["questions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|q| q["id"] == answer["question_id"])
                    .ok_or("Unknown question")?;
                let mut labels = Vec::new();
                for id in answer["selected_option_ids"]
                    .as_array()
                    .ok_or("Invalid selected options")?
                {
                    let option = item["options"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|o| o["id"] == *id)
                        .ok_or("Unknown answer option")?;
                    labels.push(option["label"].clone());
                }
                if item["allow_other"] != true && !string(answer, "text").is_empty() {
                    return Err("Choose an approval outcome".into());
                }
                answers.push(json!({"id":answer["question_id"],"selected":labels,"custom":string(answer,"text")}));
            }
            rpc(
                &record,
                "answer",
                json!({"sessionId":record["conversation_id"],"id":request["question_id"],"generation":inspected["generation"],"answers":answers}),
            )?;
            println!(
                "{}",
                json!({"status":"answered","request_id":request["request_id"],"name":name,"question_id":request["question_id"],"question_hash":request["expected_question_hash"],"run_id":record["run_id"],"conversation_id":record["conversation_id"]})
            );
        }
        "pause" | "resume" => {
            if args.iter().any(|v| v == "--dry-run") {
                println!("{command} official dsh {name}");
                return Ok(0);
            }
            if command == "resume" {
                ensure_host(&record)?;
            }
            if command == "resume" {
                create_session(&mut record)?;
            } else {
                rpc(
                    &record,
                    command,
                    json!({"sessionId":record["conversation_id"]}),
                )?;
            }
            record["paused"] = json!(command == "pause");
            if command == "resume" {
                record["run_id"] = json!(uuid::Uuid::new_v4().to_string());
            }
            save(&mut record)?;
        }
        "rename" => {
            let new = args.get(1).ok_or("Choose a new session name")?;
            if !new.starts_with("dsh/")
                || new.split('/').count() != 3
                || new.contains(['\n', '\r', ':'])
                || new.chars().any(char::is_whitespace)
            {
                return Err("Invalid DeepSeek session name".into());
            }
            if exists(new) {
                return Err("This name is already used".into());
            }
            if args.iter().any(|v| v == "--dry-run") {
                return Ok(0);
            }
            if legacy_fork_unconfirmed(&record)? { return Err("Resolve the original legacy fork before renaming this session".into()); }
            PROVIDER.prepare_label(&record, new)?;
            rpc(
                &record,
                "rename",
                json!({"sessionId":record["conversation_id"],"title":new.rsplit('/').next()}),
            )?;
            notifications::anchor(&mut record);
            record["name"] = json!(new);
            if storage::in_id_layout(&record) { recovery::rename_job_in_place(&record,new)?; } else { recovery::rename_job(name,new,string(&record,"run_id"))?; }
            save(&mut record)?;
            if !storage::in_id_layout(&record) { archive::remove_synced(&binding_path(name))?; }
            // The rename is done; losing read marks only shows a reply as unread again.
            if !storage::in_id_layout(&record) { let _ = read_marks::rename_owner(name, new); }
            println!(
                "{}",
                json!({"old_name":name,"name":new,"new_name":new,"run_id":record["run_id"],"conversation_id":record["conversation_id"]})
            );
        }
        "forget" => {
            if args.iter().any(|v| v == "--dry-run") {
                return Ok(0);
            }
            rpc(
                &record,
                "pause",
                json!({"sessionId":record["conversation_id"]}),
            )?;
            remove(&record)?;
        }
        "exists" => return Ok(0),
        "schedules" => println!("{}", schedules(name, &record, &args[1..])?),
        _ => {
            return Err(format!(
                "{command} is not supported for native DeepSeek sessions yet"
            ))
        }
    }
    Ok(0)
}

// DeepSeek is a Phase 2 non-goal on Windows.
#[cfg(all(test, unix))]
mod provider_failure_tests {
    use super::*;
    #[test]
    fn account_defaults_reach_only_the_created_generation_once() {
        type Calls = Vec<(String, Value)>;
        let run = |answer: &dyn Fn(&str) -> Result<Value>, persist_ok: bool| {
            let mut record = json!({"conversation_id":"session-fixture"});
            let (mut calls, mut saved): (Calls, Vec<Value>) = (Vec::new(), Vec::new());
            let warning = apply_account_defaults_with(
                &mut record,
                (&json!("generation-a"), "bypass"),
                "route/model",
                "max",
                |method, params| {
                    calls.push((method.into(), params));
                    answer(method)
                },
                |r| {
                    saved.push(r["account_defaults"]["status"].clone());
                    if persist_ok { Ok(()) } else { Err("read-only binding".into()) }
                },
            );
            (record, calls, saved, warning)
        };
        let selected = json!({"provider":"route","model":"model","reasoningEffort":"max"});
        let applied = |method: &str| Ok(if method == "ping" { json!({"generation":"generation-a"}) }
            else { json!({"scope":"session","selected":selected}) });
        // Pending is durable before the one scoped settings call on the same generation.
        let (record, calls, saved, warning) = run(&applied, true);
        assert!(warning.is_none());
        assert_eq!(saved, [json!("pending"), json!("applied")]);
        assert_eq!(calls.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), ["ping", "settings"]);
        assert_eq!(calls[1].1, json!({"sessionId":"session-fixture","provider":"route","model":"model",
            "generation":"generation-a","permissionMode":"bypass","reasoningEffort":"max"}));
        assert_eq!(record["account_defaults"]["selected"], selected);
        // A replaced host never receives settings: that would resume the session.
        let moved = |_: &str| Ok(json!({"generation":"generation-b"}));
        let (record, calls, saved, warning) = run(&moved, true);
        assert_eq!(calls.len(), 1);
        assert_eq!(saved, [json!("pending"), json!("failed")]);
        assert!(warning.unwrap().contains("not applied"));
        assert!(record["account_defaults"]["detail"].as_str().unwrap().contains("host changed"));
        // A refusal is definite; a lost reply may have applied. Neither retries.
        for (error, status) in [
            ("This model is unavailable for the native account", "failed"),
            (HOST_UNAVAILABLE, "failed"),
            ("Native request delivery uncertain: timed out", "uncertain"),
        ] {
            let answer = |method: &str| if method == "ping" { Ok(json!({"generation":"generation-a"})) } else { Err(error.to_owned()) };
            let (record, calls, saved, warning) = run(&answer, true);
            assert_eq!(calls.len(), 2, "{error}");
            assert_eq!(saved, [json!("pending"), json!(status)]);
            assert_eq!(record["account_defaults"]["status"], status);
            assert!(warning.is_some());
        }
        // Without a durable pending record nothing is sent.
        let (_, calls, saved, warning) = run(&applied, false);
        assert!(calls.is_empty());
        assert_eq!(saved, [json!("pending")]);
        assert!(warning.unwrap().contains("not applied"));
    }

    #[test]
    fn account_catalog_lists_native_routes_and_the_native_default() {
        let catalog = json!({"default":{"provider":"deepseek-official","model":"deepseek-flash"},
            "groups":[{"id":"deepseek-official","name":"DeepSeek","models":[{"id":"deepseek-flash","name":"DeepSeek-V4.1-Flash",
            "reasoning":{"efforts":[{"id":"off"},{"id":"low"},{"id":"high"},{"id":"max"}],"defaultEffort":"high"}}]}]});
        let models = account_models_from(&catalog);
        assert_eq!(models["models"], json!([{"id":"deepseek-official/deepseek-flash","label":"DeepSeek-V4.1-Flash (DeepSeek)",
            "effort_options":["off","low","high","max"]}]));
        assert_eq!(models["provider_defaults"], json!({"model":"deepseek-official/deepseek-flash","effort":""}));
        let empty = account_models_from(&Value::Null);
        assert_eq!(empty["models"], json!([]));
        assert_eq!(empty["provider_defaults"], json!({"model":"","effort":""}));
    }

    #[test]
    fn child_summaries_require_own_identity_and_one_host_generation() {
        let record=json!({"conversation_id":"parent"});
        let mut value=json!({"subagents":{"active":{"state":"unknown"},"idle":{"state":"unknown"},"foreign":{"state":"unknown"}}});
        let list=json!({"generation":"host-one","items":[
            {"sessionId":"active","parentSessionId":"parent","origin":"subagent","running":true,"agentAvailable":true},
            {"sessionId":"idle","parentSessionId":"parent","origin":"subagent","running":false,"agentAvailable":false},
            {"sessionId":"foreign","parentSessionId":"other","origin":"subagent","running":false,"agentAvailable":true}]});
        child_summaries(&record,&mut value,&list,&json!("host-one"));
        assert_eq!(value["subagents"]["active"]["state"],"working");
        assert_eq!(value["subagents"]["idle"]["state"],"finished");
        assert_eq!(value["subagents"]["foreign"]["state"],"unknown");
        assert_eq!(value["subagent_active_count"],1);assert_eq!(value["subagent_counts_complete"],false);
        let mut stale=json!({"subagents":{"active":{"state":"unknown"}}});let before=stale.clone();
        child_summaries(&record,&mut stale,&list,&json!("host-two"));assert_eq!(stale,before);
    }
    #[test]
    fn failed_native_turn_needs_attention_without_confusing_tool_results() {
        let record =
            json!({"name":"dsh/test/session","conversation_id":"session-one","run_id":"run"});
        let row = json!({"agentAvailable":true,"running":false});
        let failed = json!({"event":{"type":"turn/end","seq":12,"time":100000,"data":{"reason":{"kind":"error","error":{"code":"insufficient_quota","message":"Insufficient balance"}}}}});
        let mut records = json!([{"event":{"type":"tool/result","seq":11,"data":{"message":{"content":"model at capacity"}}}},failed]);
        let normalized = normalize(&record, &row, &json!({"page":{"records":records}}));
        assert_eq!(normalized["phase"], "error");
        assert_eq!(normalized["activity_summary"], "Usage limit reached");
        assert_eq!(normalized["attention_id"], "dsh_native:dsh-error-12");
        assert_eq!(
            native_events(&records).last().unwrap()["type"],
            "StopFailure"
        );
        records
            .as_array_mut()
            .unwrap()
            .push(json!({"event":{"type":"turn/start","seq":13}}));
        assert!(latest_failure(&records).is_null());
        assert_eq!(
            normalize(&record, &row, &json!({"page":{"records":records}}))["phase"],
            "idle"
        );
        records.as_array_mut().unwrap().pop();
        let mut working = row.clone();
        working["running"] = json!(true);
        assert_eq!(
            normalize(&record, &working, &json!({"page":{"records":records}}))["phase"],
            "working"
        );
        let mut stopped = row.clone();
        stopped["agentAvailable"] = json!(false);
        assert_ne!(
            normalize(&record, &stopped, &json!({"page":{"records":records}}))["phase"],
            "error"
        );
        records[1]["event"]["data"]["reason"] = json!({"kind":"completed"});
        assert!(latest_failure(&records).is_null());
        assert_eq!(native_events(&records).last().unwrap()["type"], "Stop");
        records[1]["event"]["data"]["reason"] = json!({"kind":"max-tokens"});
        assert_eq!(latest_failure(&records)["error_kind"], "context_limit");
    }
}

#[cfg(all(test, unix))]
mod native_operation_tests {
    use super::*;
    #[test]
    fn fork_needs_one_advertising_host_generation_and_a_completed_reply() {
        let record = json!({"name":"dsh/test/session"});
        let ping = json!({"generation":"host","nativeOperations":["fork","history-search","schedules"]});
        let ready = json!({"generation":"host","forkSupported":true,"forkReady":true});
        assert_eq!(fork_reason(&record, &ping, &ready), "");
        assert!(fork_reason(&record, &json!({"generation":"host","sessionActions":true}), &ready).contains("older adapter"));
        let mut other = ready.clone();
        other["generation"] = json!("replacement");
        assert!(fork_reason(&record, &ping, &other).contains("host changed"));
        let mut empty = ready.clone();
        empty["forkReady"] = json!(false);
        assert_eq!(fork_reason(&record, &ping, &empty), "Wait for a completed reply before forking");
        let pending = json!({"permissions_pending":true});
        assert!(fork_reason(&pending, &ping, &ready).contains("permission setup"));
    }
    #[test]
    fn schedule_items_keep_the_exact_native_record_for_compare_and_set() {
        let entry = json!({"id":"schedule-1","kind":"cron","title":"Deploy","prompt":"Check","expression":"0 9 * * 1-5",
            "timeZone":"UTC","scheduledAt":"2026-10-16T09:00:00.000Z","sessionId":"session-a","status":"active",
            "lastDelivery":{"messageId":"m"}});
        let item = schedule_item(&entry);
        assert_eq!(item["record"], json!({"id":"schedule-1","kind":"cron","title":"Deploy","prompt":"Check",
            "expression":"0 9 * * 1-5","timeZone":"UTC","scheduledAt":"2026-10-16T09:00:00.000Z"}));
        assert_eq!((item["recurring"].as_bool(), item["scheduled_at"].as_f64()), (Some(true), Some(1792141200.0)));
        assert_eq!(item["last_delivery"]["messageId"], "m");
        let modern = json!({"id":"schedule-2","kind":"rrule","title":"Check","prompt":"Check",
            "rrule":"DTSTART:20261105T200000Z\nRRULE:FREQ=HOURLY","scheduledAt":"2026-11-05T20:00:00.000Z",
            "target":{"kind":"session","sessionId":"original"},"involvedSessionIds":["editor","original"],
            "updatedAt":"2026-11-05T18:00:00.001Z","status":"active"});
        let item = schedule_item(&modern);
        assert_eq!(item["recurring"], true);
        let mut expected = modern;
        expected.as_object_mut().unwrap().remove("status");
        assert_eq!(item["record"], expected);
    }
}

#[cfg(all(test, unix))]
mod question_identity_tests {
    use super::*;
    /// The approval card built from `src/dsh/bridge.mjs`'s published
    /// approval names its tool for automatic-approval categories.
    #[test]
    fn automatic_approval_categories_read_the_bridge_approval_card() {
        use zerus_client_core::approvals::automatic::{categories, once, Category};
        let record = json!({"name":"dsh/test/session","conversation_id":"session-one","run_id":"run"});
        let card = |tool: &str| {
            let inspected = json!({"generation":"g1","pendingQuestions":[{"id":"q1","approval":true,"questions":[
                {"id":"decision","header":"Approval required","question":format!("Allow {tool}?"),"detail":"",
                 "options":[{"label":"Allow once"},{"label":"Reject"}]}]}]});
            question_cards(&record, &inspected).remove(0)
        };
        assert!(once("dsh", &card("bash")).is_some());
        for (tool, expected) in [("bash", Category::Shell), ("web_search", Category::Paid),
            ("mcp__docs__lookup", Category::Mcp), ("create_worktree", Category::Other)] {
            assert_eq!(categories::tool("dsh", &card(tool)).category, expected, "{tool}");
        }
    }
    #[test]
    fn pending_native_questions_have_an_exact_attention_identity() {
        let record = json!({"name":"dsh/test/session","conversation_id":"session-one","run_id":"run"});
        let row = json!({"agentAvailable":true,"running":false});
        let first = normalize(&record, &row, &json!({"pendingQuestions":[{"id":"question-1"}]}));
        assert_eq!(first["phase"], "input");
        assert_eq!(first["attention_id"], "dsh_question:question-1");
        let next = normalize(&record, &row, &json!({"pendingQuestions":[{"id":"question-2"}]}));
        assert_ne!(next["attention_id"], first["attention_id"]);
        assert!(normalize(&record, &row, &json!({}))["attention_id"].is_null());
    }
}

#[cfg(test)]
mod notification_tests {
    use super::*;
    #[test]
    fn notification_baseline_observes_only_inspect_and_list_with_matching_generation() {
        let record = json!({"name":"dsh/test/session","backend":"dsh","conversation_id":"session-A","run_id":"run-A"});
        let mut called = Vec::new();
        let (view, questions) = notification_snapshot_with(&record, |method, params| {
            called.push(method.to_owned());
            match method {
                "inspect" => { assert_eq!(params, json!({"sessionId":"session-A","includeProcesses":false})); Ok(json!({"generation":"host-1","pendingQuestions":[],"live":true})) },
                "list" => { assert_eq!(params, json!({})); Ok(json!({"generation":"host-1","items":[{"sessionId":"session-A","agentAvailable":true,"running":false}]})) },
                _ => panic!("a preference must not issue a native action"),
            }
        }).unwrap();
        assert_eq!(called, ["inspect", "list"]);
        assert_eq!(view["conversation_id"], "session-A"); assert_eq!(view["run_id"], "run-A");
        assert_eq!(view["phase"], "idle"); assert_eq!(questions, Some(Vec::new()));
        let (pending, questions) = notification_snapshot_with(&record, |method, _| Ok(if method == "inspect" {
            json!({"generation":"host","pendingQuestions":[{"id":"question-1"}],"live":true})
        } else { json!({"generation":"host","items":[{"sessionId":"session-A","agentAvailable":true,"running":false}]}) })).unwrap();
        assert_eq!(pending["phase"], "input");
        assert_eq!(pending["attention_id"], "dsh_question:question-1");
        assert_eq!(read_marks::signature(&pending, questions.as_deref()), read_marks::signature(&pending, Some(&[])));
        assert!(notification_snapshot_with(&record, |method, _| Ok(if method == "inspect" { json!({"generation":"host","pendingQuestions":[{}]}) } else { json!({"generation":"host"}) })).is_err());
        assert!(notification_snapshot_with(&record, |method, _| Ok(if method == "inspect" { json!({"generation":"old","pendingQuestions":[]}) } else { json!({"generation":"new"}) })).is_err());
        assert!(notification_snapshot_with(&record, |_, _| Ok(json!({"generation":"host"}))).is_err());
    }
}

#[cfg(all(test, unix))]
mod compatibility_tests {
    use super::*;

    /// zerus-ppd26: the session that happens to start a native host is not
    /// the session of any hosted agent, so the host never takes its markers.
    #[test]
    fn a_native_host_never_takes_the_markers_of_the_session_that_starts_it() {
        let record = json!({"agent_home":"/nonexistent/hgs-native-home"});
        let command = host_command(&record, Path::new("/nonexistent/hgs.patch.json"));
        let envs: BTreeMap<String, Option<String>> = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        for name in crate::state::backend::host::SESSION_CONTEXT.iter().chain(&[
            "HGS_TERMINAL_NAME",
            "HGS_SESSION",
            "HGS_SESSION_ID",
            "HGS_RUN_ID",
            "HGS_AGENT",
            "HGS_ACCOUNT_ID",
        ]) {
            assert_eq!(envs.get(*name), Some(&None), "{name} must be removed");
        }
        assert_eq!(
            envs.get("DSH_HOME"),
            Some(&Some("/nonexistent/hgs-native-home".to_owned()))
        );
        // The host belongs to this state root and configuration.
        for kept in ["HGS_STATE_DIR", "HGS_CONFIG_DIR"] {
            assert!(!envs.contains_key(kept), "{kept} must be inherited");
        }
    }

    #[test]
    fn startup_accepts_only_the_explicitly_reviewed_harness_releases() {
        assert!(reviewed_harness_version("0.2.1-rc.2"));
        assert!(reviewed_harness_version("0.2.1-rc.5"));
        assert!(reviewed_harness_version("0.2.2-rc.1"));
        for unknown in ["0.2.1-rc.1", "0.2.1-rc.3", "0.2.1-rc.4", "0.2.1-rc.6",
            "0.2.1-rc.50", "0.2.1", "0.2.2", "0.2.2-rc.2", "0.3.0", "0.2.1-rc.5+unreviewed", "0.2.1-rc.5\nother"] {
            assert!(!reviewed_harness_version(unknown), "{unknown}");
        }
    }

    #[test]
    fn reviewed_engine_versions_do_not_change_the_resident_adapter_identity() {
        assert!(compatible_adapter(&json!({"protocol":1,"version":"0.2.1-rc.2"})));
        for ping in [json!({"protocol":1,"version":"0.2.1-rc.5"}), json!({"protocol":1,"version":"0.2.2-rc.1"}),
            json!({"protocol":2,"version":"0.2.1-rc.2"}), json!({"protocol":1})] {
            assert!(!compatible_adapter(&ping));
        }
    }
}

#[cfg(all(test, unix))]
mod delivery_tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// zerus-1wchz: a host that takes the connection and goes away while the
    /// request is still being written. The bridge runs only a whole,
    /// newline-terminated request, so this one never ran: a definite refusal.
    #[test]
    fn a_request_cut_short_is_a_definite_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let host = std::thread::spawn(move || drop(listener.accept().unwrap()));
        // Larger than any socket buffer: the write is still running when the
        // host closes, so it fails rather than the read.
        let content = "x".repeat(16 * 1024 * 1024);
        let error = rpc_at(
            &path,
            "send",
            json!({"sessionId":"session-A","content":content}),
            Duration::from_secs(5),
        )
        .unwrap_err();
        host.join().unwrap();
        assert!(error.starts_with(INCOMPLETE), "{error}");
        assert!(!error.contains("delivery uncertain"), "{error}");
        assert_eq!(interrupt_refusal(error.clone()), error);
    }

    /// Every shipped bridge, the running ones included (a resident host is
    /// never reloaded), runs a request only from a chunk holding its newline
    /// and has no end handler. If that changes, a cut-short request may run,
    /// and `INCOMPLETE` stops being a definite refusal.
    #[test]
    fn the_bridge_runs_only_a_newline_terminated_request() {
        assert_eq!(BRIDGE.matches("socket.on('data', chunk => {").count(), 1);
        assert_eq!(
            BRIDGE.matches("if (!chunk.includes(10)) return;").count(),
            1
        );
        for handler in [
            "readline",
            "createInterface",
            "on('end'",
            "once('end'",
            "on('close'",
            "once('close'",
        ] {
            assert!(!BRIDGE.contains(handler), "{handler}");
        }
    }

    /// The production interrupt path, answered by a host: a failure after the
    /// bridge's own checks is uncertain, one of those checks stays definite.
    #[test]
    fn an_interrupt_refused_after_its_effects_is_uncertain_through_the_host() {
        let record = json!({"conversation_id":"session-A"});
        let snapshot = json!({"host_generation":"host-1","turn_started":1793000000.5});
        for (error, expected) in [
            (
                "Session not found",
                "Interrupt delivery uncertain: Session not found",
            ),
            (
                "DeepSeek turn changed; nothing was stopped",
                "DeepSeek turn changed; nothing was stopped",
            ),
            ("DeepSeek host changed", "DeepSeek host changed"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("host.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let host = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let mut request = String::new();
                BufReader::new(&socket).read_line(&mut request).unwrap();
                writeln!(socket, "{}", json!({"ok":false,"error":error})).unwrap();
                serde_json::from_str::<Value>(&request).unwrap()
            });
            let answer = interrupt_with(&record, &snapshot, |record, method, params| {
                assert_eq!(record["conversation_id"], "session-A");
                rpc_at(&path, method, params, Duration::from_secs(5))
            });
            assert_eq!(answer.unwrap_err(), expected);
            assert_eq!(
                host.join().unwrap(),
                json!({"method":"interrupt","params":{"sessionId":"session-A",
                    "generation":"host-1","turnStarted":1793000000.5}})
            );
        }
    }

    #[test]
    fn no_host_to_reach_is_a_definite_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let error = rpc_at(
            &dir.path().join("host.sock"),
            "send",
            json!({}),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(error, HOST_UNAVAILABLE);
        assert_eq!(interrupt_refusal(error), HOST_UNAVAILABLE);
    }

    /// The bridge's own checks ahead of its effects stay definite; anything
    /// after them may follow a stopped turn.
    #[test]
    fn only_an_interrupt_refused_before_its_effects_is_definite() {
        for definite in [
            "DeepSeek host changed",
            "Use the parent to manage a subagent",
            "DeepSeek turn changed; nothing was stopped",
            HOST_UNAVAILABLE,
        ] {
            assert_eq!(interrupt_refusal(definite.into()), definite);
        }
        let incomplete = format!("{INCOMPLETE}: Broken pipe (os error 32)");
        assert_eq!(interrupt_refusal(incomplete.clone()), incomplete);
        for already in [
            "Native request delivery uncertain: Broken pipe (os error 32)",
            "Invalid native response; delivery uncertain: EOF while parsing a value",
        ] {
            assert_eq!(interrupt_refusal(already.into()), already);
        }
        for after in [
            "Session not found",
            "DeepSeek host changed; refresh Processes",
            "",
        ] {
            assert_eq!(
                interrupt_refusal(after.into()),
                format!("Interrupt delivery uncertain: {after}")
            );
        }
    }
}

#[cfg(test)]
mod pending_identity_tests {
    use super::*;

    #[test]
    fn fork_intents_of_label_twins_are_distinct() {
        let source = |id, conversation| json!({"name":"dsh/example/shared","session_id":id,"conversation_id":conversation});
        let one = source("11111111-1111-4111-8111-111111111111", "session-one");
        let two = source("22222222-2222-4222-8222-222222222222", "session-two");
        assert_ne!(fork_pending_path(&one,Some("child")), fork_pending_path(&two,Some("child")));
    }
}
