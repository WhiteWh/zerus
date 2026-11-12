//! `hgs mobile`: pair, list and revoke phones with this computer's mobile
//! connector credential. The desktop asks a gateway peer over SSH, one hop, the
//! way phones reach peers through gateways. The token stays in the private
//! connector configuration and reaches curl on standard input only.
use crate::{
    cli::{Error, Result},
    config::Config,
};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
};

const USAGE: &str = "usage: hgs [@host] mobile phones|invite|revoke-phone <id> [--workspace ID --server URL] --json";
const CHANGED_RELAY: &str =
    "The mobile connection on this computer now uses another relay; refresh the phone list";
const CHANGED_WORKSPACE: &str =
    "The mobile connection on this computer now belongs to another workspace; refresh the phone list";
const MAX_CONFIG_BYTES: u64 = 65_536;
const MAX_RESPONSE_BYTES: u64 = 1_048_576;

pub fn dispatch(config: &Config, args: &[String], dry: bool) -> Result<i32> {
    if args == ["identity", "--json"] && !dry {
        let machine = crate::swarm::local_node_id(config)
            .map_err(|_| Error::new(1,"Native machine identity is unavailable"))?;
        println!("{}",json!({"schema":1,"machine_id":machine}));
        return Ok(0);
    }
    let (action, rest) = match args {
        [action, rest @ ..] if action == "phones" => (Action::Phones, rest),
        [action, rest @ ..] if action == "invite" => (Action::Invite, rest),
        [action, id, rest @ ..] if action == "revoke-phone" => {
            if !device_id(id) {
                return Err(Error::new(1, "Choose a phone by its device ID"));
            }
            (Action::Revoke(id.clone()), rest)
        }
        _ => return Err(Error::new(1, USAGE)),
    };
    // The desktop names the relay and workspace it listed, so a changed
    // connection never issues or revokes for another one.
    let pin = match rest {
        [json] if json == "--json" => None,
        [w, workspace, s, server, json]
            if w == "--workspace"
                && s == "--server"
                && json == "--json"
                && !matches!(action, Action::Phones) =>
        {
            if !workspace_id(workspace) {
                return Err(Error::new(1, "Choose a workspace by its ID"));
            }
            Some((workspace.clone(), server.trim_end_matches('/').to_owned()))
        }
        _ => return Err(Error::new(1, USAGE)),
    };
    if dry {
        return Err(Error::new(1, USAGE));
    }
    let Some(connection) = connection(config)? else {
        if matches!(action, Action::Phones) {
            println!("{}", json!({"configured": false}));
            return Ok(0);
        }
        return Err(Error::new(
            1,
            "This computer has no mobile connection; set it up in Settings → Mobile connection",
        ));
    };
    if pin
        .as_ref()
        .is_some_and(|(_, server)| *server != connection.server)
    {
        return Err(Error::new(1, CHANGED_RELAY));
    }
    let workspace = pin.map(|(workspace, _)| workspace);
    if connection.protected {
        println!("{}", protected_action(&connection,action,workspace.as_deref())?);
        return Ok(0);
    }
    let output = match action {
        Action::Phones => {
            let mut value = connection.call("GET", "/v1/node/devices", None, false)?;
            object(&mut value)?;
            let mut out = json!({"configured": true, "server_url": connection.server});
            out.as_object_mut()
                .unwrap()
                .extend(value.as_object().unwrap().clone());
            out
        }
        Action::Invite => {
            let body = match &workspace {
                Some(workspace) => json!({"workspace_id": workspace}),
                None => json!({}),
            };
            let mut value = connection.call(
                "POST",
                "/v1/node/invitations",
                Some(&body.to_string()),
                false,
            )?;
            if workspace
                .as_ref()
                .is_some_and(|workspace| value["workspace_id"] != json!(workspace))
            {
                return Err(Error::new(1, CHANGED_WORKSPACE));
            }
            let code = object(&mut value)?
                .get("pair_code")
                .and_then(Value::as_str)
                .filter(|code| code.len() <= 128)
                .ok_or_else(|| Error::new(1, "The relay returned an invalid invitation"))?
                .to_owned();
            let fields = value.as_object_mut().unwrap();
            fields.insert("server_url".into(), json!(connection.server));
            fields.insert(
                "pair_url".into(),
                json!(format!(
                    "zerus://pair?server={}&code={}",
                    encode(&connection.server),
                    encode(&code)
                )),
            );
            value
        }
        Action::Revoke(id) => {
            let scope = workspace
                .map(|workspace| format!("?workspace_id={workspace}"))
                .unwrap_or_default();
            connection.call(
                "DELETE",
                &format!("/v1/node/devices/{id}{scope}"),
                None,
                true,
            )?;
            json!({"revoked": true, "device_id": id})
        }
    };
    println!("{output}");
    Ok(0)
}

enum Action {
    Phones,
    Invite,
    Revoke(String),
}

struct Connection {
    server: String,
    token: String,
    insecure: bool,
    protected: bool,
    config_path: std::path::PathBuf,
}

fn connection(config: &Config) -> Result<Option<Connection>> {
    let path = config.dir.join("mobile/connector.json");
    #[cfg(unix)]
    let opened = std::fs::File::open(&path);
    #[cfg(windows)]
    let opened = crate::platform::refuse_link(std::fs::OpenOptions::new().read(true)).open(&path);
    let file = match opened {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(Error::new(
                1,
                "Cannot read the mobile connection configuration",
            ))
        }
    };
    #[cfg(windows)]
    if !crate::platform::verify_private_file(&file).unwrap_or(false) {
        return Err(Error::new(
            1,
            "The mobile connection configuration must be private to this Windows user",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
    let settings: Value = serde_json::from_slice(&bytes)
        .ok()
        .filter(|_| bytes.len() as u64 <= MAX_CONFIG_BYTES)
        .ok_or_else(|| Error::new(1, "Invalid mobile connection configuration"))?;
    let server = settings["server_url"]
        .as_str()
        .unwrap_or("")
        .trim_end_matches('/');
    let protected=settings.get("encrypted_workspace").is_some();
    let token = settings["node_token"].as_str().unwrap_or("");
    if !protected && (token.is_empty() || token.len() > 8192 || !token.bytes().all(|b| (33..=126).contains(&b))) {
        return Err(Error::new(
            1,
            "The mobile connection has no valid computer credential",
        ));
    }
    let loopback = ["http://127.0.0.1", "http://[::1]", "http://localhost"]
        .iter()
        .any(|origin| {
            server
                .strip_prefix(origin)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/']))
        });
    let insecure =
        loopback && std::env::var("HGS_MOBILE_ALLOW_INSECURE_LOCALHOST").as_deref() == Ok("1");
    let valid = server.len() <= 2048
        && (server.starts_with("https://") || insecure)
        && !server
            .bytes()
            .any(|b| b <= b' ' || b == 127 || b == b'"' || b == b'\\');
    if !valid {
        return Err(Error::new(
            1,
            "The mobile connection needs an HTTPS relay address",
        ));
    }
    Ok(Some(Connection {
        server: server.to_owned(),
        token: token.to_owned(),
        insecure,
        protected,
        config_path:path,
    }))
}

/// The connector owns protected keys, roster verification and signed changes.
/// The native CLI forwards only bounded management JSON over private pipes.
fn protected_call(connection:&Connection,action:&str,input:&Value)->Result<Value> {
    let executable=std::env::current_exe()?;
    let name=if cfg!(windows){"zerus-mobile-connector.exe"}else{"zerus-mobile-connector"};
    let sibling=executable.parent().map(|p|p.join(name));
    // Private runtime hint from the desktop, distinct from the installer's
    // HGS_MOBILE_CONNECTOR=0/1. It is executable selection, not authority.
    // A supplied selection never falls back to a stale sibling or PATH.
    let program=if let Some(selected)=std::env::var_os("ZERUS_CLIENT_MOBILE_CONNECTOR_PATH") {
        let path=std::path::PathBuf::from(selected);
        if !path.is_absolute(){return Err(Error::new(1,"The selected mobile connector path must be absolute"))}
        path
    }else{sibling.filter(|p|p.is_file()).unwrap_or_else(||name.into())};
    let mut command=Command::new(program);
    command.args(["--workspace",action,"--config"]).arg(&connection.config_path)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child=crate::platform::spawn_service_child(&mut command)
        .map_err(|_|Error::new(1,"Update the mobile connector to manage this encrypted workspace"))?;
    if let Some(mut stdin)=child.stdin.take(){stdin.write_all(input.to_string().as_bytes())?;}
    let mut bytes=Vec::new();child.stdout.take().ok_or_else(||Error::new(1,"Workspace response is unavailable"))?.take(MAX_RESPONSE_BYTES+1).read_to_end(&mut bytes)?;
    if bytes.len() as u64>MAX_RESPONSE_BYTES{let _=child.kill();let _=child.wait();return Err(Error::new(1,"Workspace response exceeded its limit"))}
    let status=child.wait()?;
    let result:Value=serde_json::from_slice(&bytes).map_err(|_|Error::new(1,"Invalid workspace management response"))?;
    if !status.success() || result["ok"]==false {
        let message=result["error"].as_str().filter(|s|s.len()<=1024&&!s.chars().any(char::is_control)).unwrap_or("Workspace operation was not confirmed; refresh before retrying");
        return Err(Error::new(1,message));
    }
    Ok(result)
}
/// The verifier's own marker in its reply (zerus-sa9qn.50).
#[cfg(unix)]
pub(crate) const ROUTE_PERSON_VERIFIER: &str = "question-route-person-v1";

/// A file's device, inode, size and modification time.
#[cfg(unix)]
type FileIdentity = (u64, u64, u64, i64, i64);
#[cfg(unix)]
fn file_identity(m: &std::fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    (m.dev(), m.ino(), m.len(), m.mtime(), m.mtime_nsec())
}

/// The connector binary this service runs, pinned (zerus-sa9qn.50, .54):
/// its canonical path, its identity when checked, and the descriptor that
/// was checked. On Linux the service executes that descriptor itself, so a
/// file put at the path after the check never runs. macOS has no descriptor
/// exec: it runs the path, so only this same user (outside the latch's
/// threat model) or root could swap the file between the check and the run
/// and back before the identity recheck.
#[cfg(unix)]
struct Pinned {
    path: std::path::PathBuf,
    identity: FileIdentity,
    /// Executed on Linux; elsewhere it keeps the checked file open.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    file: std::fs::File,
}

#[cfg(unix)]
impl Pinned {
    /// Whether the path still names the file that was checked.
    fn unchanged(&self) -> anyhow::Result<bool> {
        Ok(file_identity(&std::fs::metadata(&self.path)?) == self.identity)
    }
}

/// The desktop's absolute selection or the installed sibling of this `hgs`,
/// never a PATH lookup, canonical and pinned.
#[cfg(unix)]
fn pinned_connector() -> anyhow::Result<Pinned> {
    let name = "zerus-mobile-connector";
    let selected = match std::env::var_os("ZERUS_CLIENT_MOBILE_CONNECTOR_PATH") {
        Some(selected) => std::path::PathBuf::from(selected),
        None => std::env::current_exe()?
            .canonicalize()?
            .parent()
            .ok_or_else(|| anyhow::anyhow!("no installed connector"))?
            .join(name),
    };
    anyhow::ensure!(
        selected.is_absolute(),
        "the mobile connector path must be absolute"
    );
    pin(&selected.canonicalize()?)
}

/// Whether a directory may hold the pinned connector (zerus-sa9qn.54): owned
/// by this user or root, and not writable by group or others.
#[cfg(unix)]
fn trusted_directory(uid: u32, mode: u32, euid: u32) -> bool {
    (uid == euid || uid == 0) && mode & 0o022 == 0
}

/// Opens and checks `path`: its parent directory first, then the file
/// relative to that directory without following a final symlink. The file
/// must be a regular file of this user, not writable by group or others.
/// The descriptor is close-on-exec, so no other child inherits it.
#[cfg(unix)]
fn pin(path: &std::path::Path) -> anyhow::Result<Pinned> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        anyhow::bail!("the mobile connector path has no directory");
    };
    // Linux opens both for their identity only; the file is executed
    // through its descriptor.
    #[cfg(target_os = "linux")]
    let (directory_flags, file_flags) = (libc::O_PATH | libc::O_DIRECTORY, libc::O_PATH);
    #[cfg(not(target_os = "linux"))]
    let (directory_flags, file_flags) = (libc::O_DIRECTORY, libc::O_RDONLY);
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(directory_flags)
        .open(parent)?;
    // SAFETY: geteuid has no preconditions.
    let euid = unsafe { libc::geteuid() };
    let metadata = directory.metadata()?;
    anyhow::ensure!(
        metadata.is_dir() && trusted_directory(metadata.uid(), metadata.mode(), euid),
        "the mobile connector's directory is not this user's own"
    );
    let name = std::ffi::CString::new(name.as_bytes())?;
    // SAFETY: an open directory descriptor and a NUL-terminated name.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            file_flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    anyhow::ensure!(
        fd >= 0,
        "the mobile connector cannot be opened: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: openat returned a new descriptor that only this file owns.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && metadata.uid() == euid && metadata.mode() & 0o022 == 0,
        "the mobile connector binary is not this user's own"
    );
    Ok(Pinned {
        path: path.to_owned(),
        identity: file_identity(&metadata),
        file,
    })
}

/// Makes `command` execute the pinned descriptor itself (zerus-sa9qn.54):
/// in the child, once its standard streams are set, `execveat(fd, "",
/// AT_EMPTY_PATH)` with the same arguments and environment. Nothing runs
/// the path. The child alone clears close-on-exec on its copy, so an
/// interpreter of a script can still read it.
#[cfg(target_os = "linux")]
fn descriptor_exec(
    command: &mut Command,
    pinned: &Pinned,
    args: &[&str],
    home: Option<&std::ffi::OsStr>,
) -> anyhow::Result<()> {
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;
    struct Exec {
        fd: std::os::fd::RawFd,
        _strings: Vec<CString>,
        argv: Vec<*const libc::c_char>,
        envp: Vec<*const libc::c_char>,
    }
    // SAFETY: the pointers point into `_strings`, which moves with them and
    // never changes; the child only reads them.
    unsafe impl Send for Exec {}
    unsafe impl Sync for Exec {}
    impl Exec {
        /// In the child after fork: only fcntl and execveat, both
        /// async-signal-safe, and no allocation. It returns only on failure,
        /// which refuses the spawn.
        fn run(&self) -> std::io::Result<()> {
            // SAFETY: an open descriptor and NUL-terminated arrays of
            // NUL-terminated strings that outlive the call.
            unsafe {
                if libc::fcntl(self.fd, libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::syscall(
                    libc::SYS_execveat,
                    self.fd,
                    c"".as_ptr(),
                    self.argv.as_ptr(),
                    self.envp.as_ptr(),
                    libc::AT_EMPTY_PATH,
                );
            }
            Err(std::io::Error::last_os_error())
        }
    }
    let mut strings = vec![CString::new(pinned.path.as_os_str().as_bytes())?];
    for arg in args {
        strings.push(CString::new(*arg)?);
    }
    let argc = strings.len();
    if let Some(home) = home {
        strings.push(CString::new([b"HOME=", home.as_bytes()].concat())?);
    }
    let pointers = |strings: &[CString]| {
        let mut pointers: Vec<_> = strings.iter().map(|s| s.as_ptr()).collect();
        pointers.push(std::ptr::null());
        pointers
    };
    let exec = Exec {
        fd: pinned.file.as_raw_fd(),
        argv: pointers(&strings[..argc]),
        envp: pointers(&strings[argc..]),
        _strings: strings,
    };
    // SAFETY: the closure is async-signal-safe (see `Exec::run`).
    unsafe {
        command.pre_exec(move || exec.run());
    }
    Ok(())
}

/// Runs the pinned connector with `args` and `input` on stdin, at most
/// `limit` and `bytes` of stdout, with a cleared environment (HOME kept);
/// anything else is an error.
#[cfg(unix)]
fn bounded_run(
    pinned: &Pinned,
    args: &[&str],
    input: &[u8],
    limit: std::time::Duration,
    bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    use std::time::Instant;
    let mut command = Command::new(&pinned.path);
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let home = std::env::var_os("HOME");
    if let Some(home) = &home {
        command.env("HOME", home);
    }
    #[cfg(target_os = "linux")]
    descriptor_exec(&mut command, pinned, args, home.as_deref())?;
    let mut child = crate::platform::spawn_service_child(&mut command)?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input);
    }
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("no connector output"))?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout).take(bytes as u64 + 1).read_to_end(&mut out);
        out
    });
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("the mobile connector timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let out = reader
        .join()
        .map_err(|_| anyhow::anyhow!("no connector output"))?;
    anyhow::ensure!(
        status.success() && out.len() <= bytes,
        "the mobile connector refused"
    );
    Ok(out)
}

/// The person's latch statement of a phone (zerus-sa9qn.50), verified by
/// this computer's own connector against its verified membership head: the
/// desktop service runs the pinned binary itself, so the statement's
/// forwarder grants nothing. Any failure, timeout (0.4 s for the version,
/// 1.2 s for the verification) or output other than the exact verified reply
/// is an error, which the caller treats as a refusal.
#[cfg(unix)]
pub(crate) fn verify_route_person(config: &Config, input: &Value) -> anyhow::Result<Value> {
    use std::time::Duration;
    let Some(connection) = connection(config).map_err(|e| anyhow::anyhow!("{e}"))? else {
        anyhow::bail!("no mobile connection");
    };
    anyhow::ensure!(connection.protected, "not an encrypted workspace");
    let pinned = pinned_connector()?;
    // The service answers its request lane within the client's 2 s, so both
    // runs together stay well inside it; a slow verifier is a refusal.
    let version = bounded_run(
        &pinned,
        &["--version"],
        b"",
        Duration::from_millis(400),
        256,
    )?;
    let version = String::from_utf8(version)?;
    anyhow::ensure!(
        version
            .strip_prefix("zerus-mobile-connector ")
            .and_then(|v| v.strip_suffix('\n'))
            .is_some_and(|v| {
                let parts: Vec<&str> = v.split('.').collect();
                parts.len() == 3
                    && parts.iter().all(|p| {
                        !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit())
                    })
            }),
        "not the mobile connector"
    );
    let config_path = connection
        .config_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("invalid connector configuration path"))?;
    let out = bounded_run(
        &pinned,
        &[
            "--workspace",
            "verify-question-route-person",
            "--config",
            config_path,
        ],
        input.to_string().as_bytes(),
        Duration::from_millis(1200),
        65_536,
    )?;
    anyhow::ensure!(pinned.unchanged()?, "the mobile connector binary changed");
    let reply: Value = serde_json::from_slice(&out)?;
    anyhow::ensure!(
        reply.as_object().is_some_and(|o| o.len() == 3)
            && reply["ok"] == true
            && reply["verifier"] == ROUTE_PERSON_VERIFIER
            && reply["verified"] == input["statement"],
        "the statement was not verified"
    );
    Ok(reply)
}
/// Windows gateways do not offer the phone's latch statement.
#[cfg(not(unix))]
pub(crate) fn verify_route_person(_: &Config, _: &Value) -> anyhow::Result<Value> {
    anyhow::bail!("the phone's latch statement is not offered on Windows")
}

fn protected_action(connection:&Connection,action:Action,pin:Option<&str>)->Result<Value> {
    let status=protected_call(connection,"status",&json!({}))?;
    if status["state"]!="active" || status["encrypted"]!=true || status["server_url"]!=connection.server {
        return Err(Error::new(1,"Encrypted workspace state is unavailable; plaintext fallback is disabled"));
    }
    if pin.is_some_and(|id|status["workspace_id"]!=id){return Err(Error::new(1,CHANGED_WORKSPACE))}
    let mut input=json!({"expected_revision":status["config_revision"]});
    match action {
        Action::Phones=>{
            let roster=protected_call(connection,"list-devices",&input)?;
            let devices=roster["devices"].as_array().ok_or_else(||Error::new(1,"Invalid verified workspace roster"))?.iter().filter(|d|d["role"]=="phone").map(|device|{
                let id=device["device_id"].as_str().unwrap_or("");
                json!({"id":id,"name":format!("Phone {}",id.chars().take(8).collect::<String>()),"platform":"phone","paired_at":null,"last_seen_at":null,"added_by":null})
            }).collect::<Vec<_>>();
            Ok(json!({"configured":true,"encrypted":true,"server_url":connection.server,"workspace_id":status["workspace_id"],"workspace_name":status["workspace_name"],"devices":devices}))
        },
        Action::Invite=>{
            let mut value=protected_call(connection,"invite-phone",&input)?;
            let code=value["code"].as_str().filter(|c|c.starts_with("ze1.")&&c.len()<=4096).ok_or_else(||Error::new(1,"Invalid protected phone invitation"))?.to_owned();
            value["pair_code"]=json!(code);value["workspace_id"]=status["workspace_id"].clone();value["workspace_name"]=status["workspace_name"].clone();
            value["pair_url"]=json!(format!("zerus://pair?server={}&code={}",encode(&connection.server),encode(&code)));
            value["encrypted"]=json!(true);Ok(value)
        },
        Action::Revoke(id)=>{
            let roster=protected_call(connection,"list-devices",&input)?;
            if !roster["devices"].as_array().is_some_and(|rows|rows.iter().any(|d|d["device_id"]==id&&d["role"]=="phone")){
                return Err(Error::new(1,"That phone is not active in this verified workspace"));
            }
            input["device_id"]=json!(id);protected_call(connection,"revoke-device",&input)?;
            Ok(json!({"revoked":true,"device_id":id,"encrypted":true}))
        }
    }
}

impl Connection {
    /// One bounded request: no curlrc (`-q` must come first), so no redirects
    /// or traces from a user's defaults; no environment proxies (like the
    /// connector); and the bearer token only in curl's configuration on stdin.
    fn call(&self, method: &str, path: &str, body: Option<&str>, revoke: bool) -> Result<Value> {
        let mut command = Command::new("curl");
        command.args([
            "-q",
            "--silent",
            "--show-error",
            "--noproxy",
            "*",
            "--proto",
        ]);
        command.arg(if self.insecure {
            "=http,https"
        } else {
            "=https"
        });
        command.args([
            "--connect-timeout",
            "5",
            "--max-time",
            "20",
            "--max-filesize",
            "1048576",
        ]);
        command.args([
            "--config",
            "-",
            "--write-out",
            "\n%{http_code}",
            "--request",
            method,
        ]);
        // Schannel ignores curl's CA environment override; pass the explicitly
        // selected bundle without changing default native trust or verification.
        #[cfg(windows)]
        apply_ca_bundle(&mut command, std::env::var_os("CURL_CA_BUNDLE").as_deref());
        if let Some(body) = body {
            command.args(["--header", "Content-Type: application/json", "--data", body]);
        }
        command.arg(format!("{}{path}", self.server));
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = crate::platform::spawn_service_child(&mut command)
            .map_err(|_| Error::new(1, "Could not reach the relay: curl is not installed"))?;
        let header = self.token.replace('\\', "\\\\").replace('"', "\\\"");
        let written = child
            .stdin
            .take()
            .map(|mut stdin| writeln!(stdin, "header = \"Authorization: Bearer {header}\""));
        let mut stdout = Vec::new();
        child
            .stdout
            .take()
            .unwrap()
            .take(MAX_RESPONSE_BYTES + 16)
            .read_to_end(&mut stdout)?;
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .take(4096)
            .read_to_string(&mut stderr)
            .ok();
        let status = child.wait()?;
        if !status.success() || !matches!(written, Some(Ok(()))) {
            let reason = stderr.lines().next().unwrap_or("").trim();
            return Err(Error::new(
                1,
                format!("Could not reach the relay: {reason}")
                    .trim_end_matches([':', ' '])
                    .to_owned(),
            ));
        }
        let text = String::from_utf8_lossy(&stdout);
        let (body, code) = text.rsplit_once('\n').unwrap_or(("", &text));
        match code.trim() {
            "200" => serde_json::from_str(body)
                .map_err(|_| Error::new(1, "The relay returned an invalid response")),
            "401" | "403" => Err(Error::new(
                1,
                "The relay rejected this computer's credential",
            )),
            "404" if revoke => Err(Error::new(
                1,
                "That phone is not linked to this workspace; it may already be revoked",
            )),
            "404" => Err(Error::new(
                1,
                "The relay does not support phone management yet; update it",
            )),
            "409" => Err(Error::new(1, CHANGED_WORKSPACE)),
            "429" => Err(Error::new(
                1,
                "Too many requests to the relay; try again in a minute",
            )),
            "503" => Err(Error::new(1, "The relay is temporarily unavailable")),
            other => Err(Error::new(1, format!("The relay returned HTTP {other}"))),
        }
    }
}

#[cfg(any(windows, test))]
fn apply_ca_bundle(command: &mut Command, bundle: Option<&std::ffi::OsStr>) {
    if let Some(path) = bundle.filter(|path| !path.is_empty()) {
        command.arg("--cacert").arg(path);
    }
}

fn object(value: &mut Value) -> Result<&serde_json::Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| Error::new(1, "The relay returned an invalid response"))
}

fn workspace_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn device_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

/// Percent-encodes everything but RFC 3986 unreserved characters.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_or_empty_windows_ca_bundle_preserves_curl_arguments() {
        for bundle in [None, Some(std::ffi::OsStr::new(""))] {
            let mut command = Command::new("curl");
            command.args(["-q", "--proto", "=https", "--max-time", "20"]);
            let before: Vec<_> = command.get_args().map(|arg| arg.to_owned()).collect();
            apply_ca_bundle(&mut command, bundle);
            let after: Vec<_> = command.get_args().map(|arg| arg.to_owned()).collect();
            assert_eq!(after, before);
        }
    }

    #[test]
    fn windows_ca_bundle_is_one_path_argument_without_verification_bypass() {
        for path in [
            r"C:\private TLS\Δ CA.pem",
            r"\\fixture.example.test\share\CA file.pem",
        ] {
            let mut command = Command::new("curl");
            command.args([
                "-q",
                "--silent",
                "--show-error",
                "--noproxy",
                "*",
                "--proto",
                "=https",
                "--connect-timeout",
                "5",
                "--max-time",
                "20",
                "--max-filesize",
                "1048576",
                "--config",
                "-",
            ]);
            let before: Vec<_> = command.get_args().map(|arg| arg.to_owned()).collect();
            apply_ca_bundle(&mut command, Some(std::ffi::OsStr::new(path)));
            let args: Vec<_> = command.get_args().map(|arg| arg.to_owned()).collect();
            assert_eq!(&args[..before.len()], before.as_slice());
            assert_eq!(args.len(), before.len() + 2);
            assert_eq!(args[before.len()].as_os_str(), "--cacert");
            assert_eq!(
                args[before.len() + 1].as_os_str(),
                std::ffi::OsStr::new(path)
            );
            assert!(!args.iter().any(|arg| {
                ["--insecure", "-k", "--ssl-no-revoke"]
                    .iter()
                    .any(|flag| arg.as_os_str() == std::ffi::OsStr::new(flag))
            }));
        }
    }

    #[test]
    fn pairing_links_encode_the_server_and_device_ids_are_exact() {
        assert_eq!(
            encode("https://relay.example.test:8443/a b"),
            "https%3A%2F%2Frelay.example.test%3A8443%2Fa%20b"
        );
        assert!(device_id("00000000-0000-4000-8000-0000000000d1"));
        assert!(!device_id("00000000-0000-4000-8000-0000000000d"));
        assert!(!device_id("00000000-0000-4000-8000/000000000d1"));
        assert!(!device_id("../devices/00000000-0000-4000-8000-0"));
    }

    /// A private directory holding a copy of `program` as the connector.
    #[cfg(unix)]
    fn connector_copy(program: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .canonicalize()
            .unwrap()
            .join("zerus-mobile-connector");
        std::fs::copy(program, &path).unwrap();
        (dir, path)
    }

    #[cfg(unix)]
    #[test]
    fn only_a_private_connector_in_a_private_directory_is_pinned() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, path) = connector_copy("/bin/echo");
        let parent = path.parent().unwrap().to_owned();
        let mode = |path: &std::path::Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
        };
        mode(&path, 0o755);
        mode(&parent, 0o755);
        assert!(pin(&path).is_ok());
        // Its directory, writable by group or others (zerus-sa9qn.54).
        for bits in [0o775, 0o757, 0o1777] {
            mode(&parent, bits);
            assert!(pin(&path).is_err(), "directory {bits:o}");
        }
        mode(&parent, 0o700);
        // The file itself.
        for bits in [0o775, 0o757] {
            mode(&path, bits);
            assert!(pin(&path).is_err(), "file {bits:o}");
        }
        mode(&path, 0o700);
        // A final symlink is not followed, even to the same file.
        let link = parent.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(pin(&link).is_err());
        // The held descriptor is close-on-exec here: no other child gets it.
        let pinned = pin(&path).unwrap();
        // SAFETY: an open descriptor.
        let flags =
            unsafe { libc::fcntl(std::os::fd::AsRawFd::as_raw_fd(&pinned.file), libc::F_GETFD) };
        assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
        // Owners this test cannot create.
        let me = 1000;
        assert!(trusted_directory(me, 0o40700, me));
        assert!(trusted_directory(0, 0o40755, me));
        assert!(!trusted_directory(1001, 0o40700, me));
        assert!(!trusted_directory(me, 0o40720, me));
        assert!(!trusted_directory(0, 0o40702, me));
    }

    /// The checked descriptor runs even when another program takes its path
    /// after the check (zerus-sa9qn.54).
    #[cfg(target_os = "linux")]
    #[test]
    fn the_checked_connector_runs_even_when_its_path_is_replaced() {
        let (dir, path) = connector_copy("/bin/echo");
        let pinned = pin(&path).unwrap();
        let other = dir.path().join("other");
        std::fs::copy("/bin/true", &other).unwrap();
        std::fs::rename(&other, &path).unwrap();
        let out = bounded_run(
            &pinned,
            &["checked"],
            b"",
            std::time::Duration::from_secs(10),
            256,
        )
        .unwrap();
        assert_eq!(out, b"checked\n");
        assert!(!pinned.unchanged().unwrap(), "the path names another file");
    }

    /// A script runs through its descriptor too: its interpreter reads the
    /// copy the child keeps. The environment is cleared but for HOME.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_pinned_script_runs_through_its_descriptor_with_a_cleared_environment() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .canonicalize()
            .unwrap()
            .join("zerus-mobile-connector");
        std::fs::write(
            &path,
            "#!/bin/sh\nread line\nprintf '%s|%s|%s|%s\\n' \"$1\" \"$line\" \"${HOME-unset}\" \"${CARGO_MANIFEST_DIR-unset}\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let pinned = pin(&path).unwrap();
        let out = bounded_run(
            &pinned,
            &["arg"],
            b"input\n",
            std::time::Duration::from_secs(10),
            256,
        )
        .unwrap();
        let home = std::env::var("HOME").unwrap_or_else(|_| "unset".into());
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("arg|input|{home}|unset\n")
        );
    }
}
