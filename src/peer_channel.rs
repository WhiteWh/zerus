//! WhiteWh fork: one lasting SSH connection to each Windows peer.
//!
//! Windows OpenSSH cannot share a connection between ssh.exe processes
//! (ControlMaster needs Unix sockets), so every `hgs @peer` command paid for a
//! TCP handshake, key exchange and a new sshd session process, several seconds
//! over a relayed tailnet path. A broker (`hgs __peer broker <alias>`) holds one
//! ssh connection running `hgs __peer serve` on the peer and takes requests from
//! this computer's hgs processes over a named pipe only this user can open.
//!
//! The peer runs each request as its own `hgs.exe` with the request's argv, so
//! arguments never pass through `cmd.exe`. A broker leaves after ten idle
//! minutes or when its connection ends; the next request starts a new one.
//! Interactive commands and anything not listed in [`routed`] keep their own
//! ssh process. A request that never reached the broker falls back to that path.
#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
use crate::{config::Config, platform};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{self, Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

/// The largest frame either side accepts: a JSON object with base64 output.
const MAX_FRAME: usize = 96 << 20;
/// Requests the peer runs at once; more wait their turn.
const PARALLEL: usize = 8;
/// A broker without requests for this long leaves.
const IDLE: Duration = Duration::from_secs(10 * 60);
/// How long a request may take before its client gives up on it.
const REQUEST: Duration = Duration::from_secs(15 * 60);
/// How long the first request waits for a new broker to connect.
const START: Duration = Duration::from_secs(25);
/// After a broker could not connect, requests use their own ssh this long.
const RETRY: Duration = Duration::from_secs(10 * 60);

/// Marks a broker of `alias` that could not connect, such as to a peer whose
/// hgs predates the channel; requests skip the channel until it is old.
fn failed_marker(config: &Config, alias: &str) -> PathBuf {
    config.dir.join("peer-channel").join(format!("{alias}.failed"))
}
fn failed_recently(config: &Config, alias: &str, since: Option<std::time::SystemTime>) -> bool {
    let Ok(modified) = std::fs::metadata(failed_marker(config, alias)).and_then(|m| m.modified()) else {
        return false;
    };
    match since {
        Some(since) => modified >= since,
        None => modified.elapsed().is_ok_and(|age| age < RETRY),
    }
}

fn write_frame(writer: &mut dyn Write, value: &Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "peer frame is too large"));
    }
    let mut frame = (bytes.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&bytes);
    writer.write_all(&frame)?;
    writer.flush()
}

/// The next frame, or None at a clean end of the stream.
fn read_frame(reader: &mut dyn Read) -> io::Result<Option<Value>> {
    let mut length = [0u8; 4];
    match reader.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "peer frame is too large"));
    }
    let mut bytes = vec![0u8; length];
    reader.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn bytes(value: &Value) -> Vec<u8> {
    value.as_str().and_then(|text| STANDARD.decode(text).ok()).unwrap_or_default()
}

/// Whether `hgs @peer <command> <args>` goes through the channel, and whether it
/// reads stdin there. Only listed commands do: a command that does not read stdin
/// must not wait for its end, and the desktop leaves stdin open for those.
pub fn routed(command: &str, args: &[String]) -> Option<bool> {
    let json = args.iter().any(|arg| arg == "--json");
    match command {
        "ls" | "archives" | "inspect" | "worktrees" | "git-status" | "git-diff" | "dirs" | "project" | "analytics" => Some(false),
        "account" if matches!(args.first().map(String::as_str), Some("ls" | "inspect")) => Some(false),
        "terminal" | "send" | "settings" | "effort" | "history" | "rules" if json => Some(true),
        // WhiteWh fork: the lead's worker collecting a member's kept reports.
        "team" if json && args.first().is_some_and(|verb| verb == "outbox") => Some(true),
        _ => None,
    }
}

/// What the peer answered: its exit code and output.
pub struct Answer {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Runs `args` (the remote hgs argv) on `alias` through its broker. None when
/// no broker could take the request, so the caller may use its own ssh; an
/// error once the request may have reached the peer. The caller checks what
/// the command needs of the peer first ([`supports`]).
pub fn request(config: &Config, alias: &str, args: &[String], stdin: Option<Vec<u8>>) -> Option<Result<Answer, String>> {
    request_with(config, alias, args, stdin, true)
}

/// Whether the peer's hgs lists every marker in its help, asked through the
/// channel: no ssh connection of its own, which to a Windows computer costs
/// seconds and, twice within the six seconds a command's preparation gets,
/// failed sends to a busy peer before anything was sent. `start`: whether a
/// broker may be started for the question; a command that keeps its own ssh
/// asks only a broker that already runs. None when no broker answered, so the
/// caller asks over its own ssh; an error when the answer was lost.
pub fn supports(config: &Config, alias: &str, markers: &[(&str, bool)], start: bool) -> Option<Result<bool, String>> {
    if markers.is_empty() {
        return Some(Ok(true));
    }
    Some(request_with(config, alias, &["--help".into()], None, start)?.map(|answer| {
        answer.code == 0 && crate::remote_shell::lists(&String::from_utf8_lossy(&answer.stdout), markers)
    }))
}

#[cfg(windows)]
fn request_with(config: &Config, alias: &str, args: &[String], stdin: Option<Vec<u8>>, start: bool) -> Option<Result<Answer, String>> {
    if std::env::var_os("HGS_PEER_CHANNEL").is_some_and(|value| value == "0") {
        return None;
    }
    if failed_recently(config, alias, None) {
        return None;
    }
    let name = pipe_name(config, alias).ok()?;
    let mut stream = connect(config, &name, alias, start)?;
    let request = json!({"args": args, "stdin": stdin.map(|input| STANDARD.encode(input))});
    if write_frame(&mut stream, &request).is_err() {
        return None;
    }
    let _ = stream.set_read_timeout(Some(REQUEST));
    let answer = match read_frame(&mut stream) {
        Ok(Some(answer)) => answer,
        Ok(None) | Err(_) => return Some(Err(format!("link to {alias} dropped; check the command's result"))),
    };
    if let Some(error) = answer["error"].as_str() {
        return Some(Err(format!("{alias}: {error}")));
    }
    Some(Ok(Answer {
        code: answer["code"].as_i64().unwrap_or(1) as i32,
        stdout: bytes(&answer["stdout"]),
        stderr: bytes(&answer["stderr"]),
    }))
}

#[cfg(not(windows))]
fn request_with(_: &Config, _: &str, _: &[String], _: Option<Vec<u8>>, _: bool) -> Option<Result<Answer, String>> {
    None
}

/// The broker's pipe: this user, this configuration and this alias.
#[cfg(windows)]
fn pipe_name(config: &Config, alias: &str) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let sid = crate::host::token::user_sid()?;
    let mut hash = Sha256::new();
    hash.update(sid.as_bytes());
    hash.update([0]);
    hash.update(crate::host::endpoint::fold_case(&config.dir.to_string_lossy()).as_bytes());
    hash.update([0]);
    hash.update(alias.as_bytes());
    let digest: String = hash.finalize()[..16].iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(r"\\.\pipe\hgs-peer-{digest}"))
}

/// A connection to the broker of `alias`, starting one when none runs and
/// `start` allows it.
#[cfg(windows)]
fn connect(config: &Config, name: &str, alias: &str, start: bool) -> Option<crate::host::pipe::PipeStream> {
    use crate::host::pipe::{connect, ConnectError};
    match connect(name, Instant::now() + Duration::from_millis(500)) {
        Ok(stream) => return Some(stream),
        Err(ConnectError::NoHost) if start => {}
        Err(_) => return None,
    }
    let program = std::env::current_exe().ok()?;
    let mut command = Command::new(program);
    command
        .args(["__peer", "broker", alias])
        // Never the folder of the program that asked: a lasting broker would keep
        // a portable installation in use for its updater.
        .current_dir(&config.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let started = std::time::SystemTime::now();
    platform::spawn_detached(&mut command).ok()?;
    let deadline = Instant::now() + START;
    while Instant::now() < deadline && !failed_recently(config, alias, Some(started)) {
        match connect(name, Instant::now() + Duration::from_millis(500)) {
            Ok(stream) => return Some(stream),
            Err(ConnectError::NoHost | ConnectError::Busy) => thread::sleep(Duration::from_millis(100)),
            Err(_) => return None,
        }
    }
    None
}

/// `hgs __peer serve|broker <alias>`.
pub fn command(config: &Config, args: &[String]) -> crate::cli::Result<i32> {
    let failed = |error: String| crate::cli::Error::new(1, error);
    match args.first().map(String::as_str) {
        Some("serve") => serve().map(|()| 0).map_err(|e| failed(e.to_string())),
        #[cfg(windows)]
        Some("broker") if args.len() == 2 => broker(config, &args[1]).map(|()| 0).map_err(|error| {
            let marker = failed_marker(config, &args[1]);
            if let Some(parent) = marker.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&marker, format!("{error}
"));
            failed(error)
        }),
        _ => {
            let _ = config;
            Err(failed("usage: hgs __peer serve | broker ALIAS".into()))
        }
    }
}

/// The hgs.exe a request runs: the installed one, so that a request after an
/// update of this computer runs the new build, not the image this server began as.
fn installed() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    let file = if cfg!(windows) { "hgs.exe" } else { "hgs" };
    home.map(|home| home.join(".local").join("bin").join(file))
        .filter(|path| path.is_file())
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from(file))
}

/// The peer's end: requests on stdin, answers on stdout, in any order.
fn serve() -> io::Result<()> {
    let output: Arc<Mutex<io::Stdout>> = Arc::new(Mutex::new(io::stdout()));
    write_frame(&mut *output.lock().unwrap(), &json!({"hello": 1, "version": env!("CARGO_PKG_VERSION")}))?;
    let running = Arc::new((Mutex::new(0usize), Condvar::new()));
    let program = installed();
    let mut input = io::stdin().lock();
    while let Some(request) = read_frame(&mut input)? {
        {
            let (count, ready) = &*running;
            let mut count = count.lock().unwrap();
            while *count >= PARALLEL {
                count = ready.wait(count).unwrap();
            }
            *count += 1;
        }
        let (output, running, program) = (output.clone(), running.clone(), program.clone());
        thread::spawn(move || {
            let answer = run(&program, &request);
            let _ = write_frame(&mut *output.lock().unwrap(), &answer);
            let (count, ready) = &*running;
            *count.lock().unwrap() -= 1;
            ready.notify_one();
        });
    }
    Ok(())
}

fn run(program: &PathBuf, request: &Value) -> Value {
    let id = request["id"].clone();
    let args: Vec<String> = request["args"]
        .as_array()
        .map(|args| args.iter().filter_map(|arg| arg.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    if args.is_empty() || args[0].starts_with("__") {
        return json!({"id": id, "error": "this request cannot run on the peer"});
    }
    let input = request["stdin"].as_str().map(|_| bytes(&request["stdin"]));
    let mut command = Command::new(program);
    command
        .args(&args)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return json!({"id": id, "error": format!("cannot run hgs: {error}")}),
    };
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    match child.wait_with_output() {
        Ok(output) => json!({
            "id": id,
            "code": output.status.code().unwrap_or(1),
            "stdout": STANDARD.encode(&output.stdout),
            "stderr": STANDARD.encode(&output.stderr),
        }),
        Err(error) => json!({"id": id, "error": format!("hgs did not finish: {error}")}),
    }
}

/// This computer's end for `alias`: one ssh connection and the pipe its
/// clients use.
#[cfg(windows)]
fn broker(config: &Config, alias: &str) -> Result<(), String> {
    use crate::host::{pipe::PipeListener, token::Identity};
    if !crate::windows_peer::is_windows(config, alias) || alias.starts_with('-') || alias.chars().any(char::is_whitespace) {
        return Err(format!("{alias} is not a Windows peer"));
    }
    let name = pipe_name(config, alias).map_err(|e| e.to_string())?;
    let mut ssh = platform::ssh();
    ssh.args(crate::machines::ssh_options(config, alias)).args([
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=15",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        alias,
    ]);
    ssh.arg(crate::windows_peer::command(&["__peer".into(), "serve".into()]).map_err(|e| e.message)?);
    let mut child = platform::spawn_ssh(&mut ssh, true, true, true).map_err(|e| format!("cannot start ssh: {e}"))?;
    let mut from_peer = child.stdout.take().ok_or("ssh has no output")?;
    let to_peer = Arc::new(Mutex::new(child.stdin.take().ok_or("ssh has no input")?));
    if let Some(mut errors) = child.stderr.take() {
        thread::spawn(move || {
            let _ = io::copy(&mut errors, &mut io::sink());
        });
    }
    // The peer greets first: an older hgs there answers with an error instead.
    let (greeted, hello) = mpsc::channel();
    let reader = thread::spawn(move || {
        let first = read_frame(&mut from_peer);
        let _ = greeted.send(first.map(|frame| frame.is_some_and(|frame| frame["hello"] == 1)));
        from_peer
    });
    match hello.recv_timeout(START) {
        Ok(Ok(true)) => {}
        _ => {
            let _ = child.child.kill();
            return Err(format!("{alias} did not start its end of the channel"));
        }
    }
    let mut from_peer = reader.join().map_err(|_| "channel reader failed")?;
    let _ = std::fs::remove_file(failed_marker(config, alias));
    let listener = match PipeListener::bind(&name, &Identity::current().map_err(|e| e.to_string())?) {
        Ok(listener) => Arc::new(listener),
        // Another broker for the same peer won the race.
        Err(_) => {
            let _ = child.child.kill();
            return Ok(());
        }
    };
    let pending: Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>> = Arc::default();
    let active = Arc::new(AtomicUsize::new(0));
    let last = Arc::new(Mutex::new(Instant::now()));
    {
        let pending = pending.clone();
        let listener = listener.clone();
        thread::spawn(move || {
            while let Ok(Some(answer)) = read_frame(&mut from_peer) {
                let id = answer["id"].as_u64().unwrap_or(0);
                if let Some(sender) = pending.lock().unwrap().remove(&id) {
                    let _ = sender.send(answer);
                }
            }
            // The connection ended: waiting clients get an error, new ones a fresh broker.
            pending.lock().unwrap().clear();
            listener.close();
        });
    }
    {
        let (active, last, listener) = (active.clone(), last.clone(), listener.clone());
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(15));
            if active.load(Ordering::SeqCst) == 0 && last.lock().unwrap().elapsed() >= IDLE {
                listener.close();
                return;
            }
        });
    }
    let next = Arc::new(AtomicU64::new(1));
    while let Ok(mut stream) = listener.accept() {
        let (pending, to_peer, next, active, last) = (pending.clone(), to_peer.clone(), next.clone(), active.clone(), last.clone());
        active.fetch_add(1, Ordering::SeqCst);
        thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
            if let Ok(Some(mut request)) = read_frame(&mut stream) {
                let id = next.fetch_add(1, Ordering::SeqCst);
                request["id"] = json!(id);
                let (sender, answer) = mpsc::channel();
                pending.lock().unwrap().insert(id, sender);
                let answer = if write_frame(&mut *to_peer.lock().unwrap(), &request).is_ok() {
                    answer.recv_timeout(REQUEST).unwrap_or_else(|_| json!({"error": "the connection to the peer ended"}))
                } else {
                    json!({"error": "the connection to the peer ended"})
                };
                pending.lock().unwrap().remove(&id);
                let _ = write_frame(&mut stream, &answer);
            }
            *last.lock().unwrap() = Instant::now();
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
    // Answers still on their way get a moment before ssh ends with this process.
    let deadline = Instant::now() + Duration::from_secs(5);
    while active.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.child.kill();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_refuse_oversized_lengths() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &json!({"id": 3, "args": ["ls", "--local", "--json"]})).unwrap();
        write_frame(&mut buffer, &json!({"hello": 1})).unwrap();
        let mut reader = buffer.as_slice();
        assert_eq!(read_frame(&mut reader).unwrap().unwrap()["id"], 3);
        assert_eq!(read_frame(&mut reader).unwrap().unwrap()["hello"], 1);
        assert!(read_frame(&mut reader).unwrap().is_none());
        let huge = ((MAX_FRAME + 1) as u32).to_le_bytes();
        assert!(read_frame(&mut huge.as_slice()).is_err());
    }

    #[test]
    fn only_listed_commands_use_the_channel() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(routed("ls", &args(&["--json"])), Some(false));
        assert_eq!(routed("terminal", &args(&["s", "--json"])), Some(true));
        assert_eq!(routed("send", &args(&["s"])), None);
        assert_eq!(routed("account", &args(&["ls", "--json"])), Some(false));
        assert_eq!(routed("account", &args(&["login", "x"])), None);
        assert_eq!(routed("claude", &args(&["dir", "--new"])), None);
    }

    #[test]
    fn the_peer_refuses_private_commands() {
        let answer = run(&installed(), &json!({"id": 1, "args": ["__peer", "serve"]}));
        assert!(answer["error"].is_string());
    }
}
