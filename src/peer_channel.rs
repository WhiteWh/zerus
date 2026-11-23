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
    collections::{HashMap, HashSet},
    io::{self, Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
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
        "team" if json && args.first().is_some_and(|verb| matches!(verb.as_str(),
            "list" | "status" | "inbox" | "events" | "waiting" | "spans" | "profile" | "test" | "builds" | "outbox"
        ) || verb == "catalog" && args.get(1).is_some_and(|op| op == "get")) => Some(true),
        // Unlike the team reads, recovery get never consumes stdin.
        "recovery" if args == ["get"] => Some(false),
        _ => None,
    }
}

/// What the peer answered: its exit code and output.
pub struct Answer {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Each individual pipe operation uses what remains of the same budget;
/// partial frame reads/writes never restart the caller's deadline.
#[cfg(windows)]
struct DeadlineStream<'a> {
    stream: &'a mut crate::host::pipe::PipeStream,
    deadline: Instant,
}

#[cfg(windows)]
impl DeadlineStream<'_> {
    fn remaining(&self) -> io::Result<Duration> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            Err(io::Error::new(io::ErrorKind::TimedOut, "peer request deadline expired"))
        } else {
            Ok(remaining)
        }
    }
}

#[cfg(windows)]
impl Read for DeadlineStream<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(bytes)
    }
}

#[cfg(windows)]
impl Write for DeadlineStream<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// Runs `args` (the remote hgs argv) on `alias` through its broker. None when
/// no broker could take the request, so the caller may use its own ssh; an
/// error once the request may have reached the peer. The caller checks what
/// the command needs of the peer first ([`supports`]).
pub fn request(config: &Config, alias: &str, args: &[String], stdin: Option<Vec<u8>>) -> Option<Result<Answer, String>> {
    request_until(config, alias, args, stdin, Instant::now() + REQUEST)
}

/// The same submission, sharing the caller's original absolute deadline.
/// Only None proves that no broker took it; an uncertain write or answer
/// must never fall back and submit the command again.
pub fn request_until(config: &Config, alias: &str, args: &[String], stdin: Option<Vec<u8>>, deadline: Instant) -> Option<Result<Answer, String>> {
    if !listed_input(args, stdin.as_deref()) {
        return None;
    }
    request_with(config, alias, args, stdin, true, deadline)
}

/// `team test` also submits/runs/settles tests. Only its two read actions
/// join the new routes; mutation requests retain their original SSH path.
fn listed_input(args: &[String], input: Option<&[u8]>) -> bool {
    if args.first().is_some_and(|arg| arg == "team") && args.get(1).is_some_and(|arg| arg == "test") {
        return input.and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
            .is_some_and(|value| matches!(value["action"].as_str(), Some("queue" | "results")));
    }
    true
}

/// A `watch --json` stream carried by the lasting channel (#19). None means
/// neither endpoint took the stream, and the caller keeps its own ssh.
/// Protocol 2 is not wired yet, so every call keeps that fallback.
pub fn watch(_config: &Config, _alias: &str, _args: &[String]) -> Option<Result<i32, String>> {
    None
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
    Some(request_with(config, alias, &["--help".into()], None, start, Instant::now() + REQUEST)?.map(|answer| {
        answer.code == 0 && crate::remote_shell::lists(&String::from_utf8_lossy(&answer.stdout), markers)
    }))
}

#[cfg(windows)]
fn request_with(config: &Config, alias: &str, args: &[String], stdin: Option<Vec<u8>>, start: bool, deadline: Instant) -> Option<Result<Answer, String>> {
    if std::env::var_os("HGS_PEER_CHANNEL").is_some_and(|value| value == "0") {
        return None;
    }
    if failed_recently(config, alias, None) {
        return None;
    }
    let name = pipe_name(config, alias).ok()?;
    let mut stream = connect(config, &name, alias, start, deadline)?;
    let request = json!({"args": args, "stdin": stdin.map(|input| STANDARD.encode(input))});
    if Instant::now() >= deadline {
        return Some(Err(format!("connection to {alias} timed out before submission")));
    }
    let mut stream = DeadlineStream { stream: &mut stream, deadline };
    if write_frame(&mut stream, &request).is_err() {
        return Some(Err(format!("link to {alias} dropped while submitting; check the command's result")));
    }
    let answer = match read_frame(&mut stream) {
        Ok(Some(answer)) => answer,
        Ok(None) | Err(_) => return Some(Err(format!("link to {alias} dropped; check the command's result"))),
    };
    if Instant::now() >= deadline {
        return Some(Err(format!("connection to {alias} timed out; check the command's result")));
    }
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
fn request_with(_: &Config, _: &str, _: &[String], _: Option<Vec<u8>>, _: bool, _: Instant) -> Option<Result<Answer, String>> {
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
fn connect(config: &Config, name: &str, alias: &str, start: bool, deadline: Instant) -> Option<crate::host::pipe::PipeStream> {
    use crate::host::pipe::{connect, ConnectError};
    if Instant::now() >= deadline {
        return None;
    }
    match connect(name, deadline.min(Instant::now() + Duration::from_millis(500))) {
        Ok(stream) => return Some(stream),
        Err(ConnectError::NoHost) if start => {}
        Err(_) => return None,
    }
    if Instant::now() >= deadline {
        return None;
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
    let deadline = deadline.min(Instant::now() + START);
    while Instant::now() < deadline && !failed_recently(config, alias, Some(started)) {
        match connect(name, deadline.min(Instant::now() + Duration::from_millis(500))) {
            Ok(stream) => return Some(stream),
            Err(ConnectError::NoHost | ConnectError::Busy) => thread::sleep(deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(100))),
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

#[cfg(windows)]
enum ClientEvent {
    Answer(Value),
    Closed,
}

#[cfg(windows)]
type Pending = Arc<Mutex<HashMap<u64, mpsc::Sender<ClientEvent>>>>;

#[cfg(windows)]
type InFlight = Arc<Mutex<HashSet<u64>>>;

/// A local client's lifetime is independent of the remote operation. When
/// its pipe closes, release its bookkeeping now and discard any later answer.
/// Never cancel or replay a command that might already have run on the peer.
#[cfg(windows)]
fn relay_request<W: Write + Send + 'static>(
    stream: &mut crate::host::pipe::PipeStream,
    mut request: Value,
    id: u64,
    pending: &Pending,
    in_flight: InFlight,
    to_peer: Arc<Mutex<W>>,
) -> io::Result<()> {
    let mut closed = stream.try_clone()?;
    // The complete request has been read. The only remaining client input
    // is closure; do not mistake a long operation for a disconnected client.
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let (sender, events) = mpsc::channel();
    let gone = sender.clone();
    thread::spawn(move || {
        let _ = closed.read(&mut [0]);
        let _ = gone.send(ClientEvent::Closed);
    });
    request["id"] = json!(id);
    pending.lock().unwrap().insert(id, sender.clone());
    let cancelled = Arc::new(AtomicBool::new(false));
    let abandoned = cancelled.clone();
    thread::spawn(move || {
        let mut writer = to_peer.lock().unwrap();
        // A closed client queued behind another write need not be submitted.
        // Once the write begins its outcome is unknown, so only discard it.
        if !abandoned.load(Ordering::SeqCst) {
            // A client can leave without cancelling the remote operation.
            // Keep the transport alive until its answer or connection EOF.
            in_flight.lock().unwrap().insert(id);
            if write_frame(&mut *writer, &request).is_err() {
                let _ = sender.send(ClientEvent::Answer(json!({"error": "the connection to the peer ended"})));
            }
        }
    });
    let answer = match events.recv_timeout(REQUEST) {
        Ok(ClientEvent::Answer(answer)) => Some(answer),
        Ok(ClientEvent::Closed) => None,
        Err(_) => Some(json!({"error": "the connection to the peer ended"})),
    };
    cancelled.store(true, Ordering::SeqCst);
    pending.lock().unwrap().remove(&id);
    let result = answer.map_or(Ok(()), |answer| write_frame(stream, &answer));
    stream.shutdown(); // wake and release the closure observer after an answer
    result
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
    let pending = Pending::default();
    let in_flight = InFlight::default();
    let active = Arc::new(AtomicUsize::new(0));
    let last = Arc::new(Mutex::new(Instant::now()));
    {
        let pending = pending.clone();
        let in_flight = in_flight.clone();
        let listener = listener.clone();
        thread::spawn(move || {
            while let Ok(Some(answer)) = read_frame(&mut from_peer) {
                let id = answer["id"].as_u64().unwrap_or(0);
                in_flight.lock().unwrap().remove(&id);
                if let Some(sender) = pending.lock().unwrap().remove(&id) {
                    let _ = sender.send(ClientEvent::Answer(answer));
                }
            }
            // The connection ended: waiting clients get an error, new ones a fresh broker.
            for (_, sender) in pending.lock().unwrap().drain() {
                let _ = sender.send(ClientEvent::Answer(json!({"error": "the connection to the peer ended"})));
            }
            in_flight.lock().unwrap().clear();
            listener.close();
        });
    }
    {
        let (active, last, listener, in_flight) = (active.clone(), last.clone(), listener.clone(), in_flight.clone());
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(15));
            if active.load(Ordering::SeqCst) == 0 && in_flight.lock().unwrap().is_empty() && last.lock().unwrap().elapsed() >= IDLE {
                listener.close();
                return;
            }
        });
    }
    let next = Arc::new(AtomicU64::new(1));
    while let Ok(mut stream) = listener.accept() {
        let (pending, in_flight, to_peer, next, active, last) = (pending.clone(), in_flight.clone(), to_peer.clone(), next.clone(), active.clone(), last.clone());
        active.fetch_add(1, Ordering::SeqCst);
        thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
            if let Ok(Some(request)) = read_frame(&mut stream) {
                let id = next.fetch_add(1, Ordering::SeqCst);
                let _ = relay_request(&mut stream, request, id, &pending, in_flight, to_peer);
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
        for verb in ["list", "status", "inbox", "events", "waiting", "spans", "profile", "test", "builds", "outbox"] {
            assert_eq!(routed("team", &args(&[verb, "--json"])), Some(true), "{verb}");
            assert_eq!(routed("team", &args(&[verb])), None, "{verb} without JSON must not consume stdin");
        }
        assert_eq!(routed("team", &args(&["catalog", "get", "--json"])), Some(true));
        for verb in ["set", "propose", "decide", "apply", "discard", "route", "designer"] {
            assert_eq!(routed("team", &args(&["catalog", verb, "--json"])), None, "catalog {verb}");
        }
        for verb in ["run", "report", "ask", "reply", "accept", "stop", "create", "settings", "unknown"] {
            assert_eq!(routed("team", &args(&[verb, "--json"])), None, "{verb} must keep its original delivery path");
        }
        assert_eq!(routed("recovery", &args(&["get"])), Some(false));
        for verb in ["set", "sync", "action", "worker", "tick"] {
            assert_eq!(routed("recovery", &args(&[verb])), None);
        }
    }

    #[test]
    fn the_peer_refuses_private_commands() {
        let answer = run(&installed(), &json!({"id": 1, "args": ["__peer", "serve"]}));
        assert!(answer["error"].is_string());
    }

    #[test]
    fn test_reads_route_without_routing_test_mutations() {
        let args = ["team", "test", "--json"].map(String::from);
        for (action, allowed) in [("queue", true), ("results", true), ("submit", false), ("force", false), ("dismiss", false), ("full-done", false), ("future-action", false)] {
            let input = serde_json::to_vec(&json!({"action": action})).unwrap();
            assert_eq!(listed_input(&args, Some(&input)), allowed, "{action}");
        }
        for input in [None, Some(b"{}".as_slice()), Some(b"invalid".as_slice())] {
            assert!(!listed_input(&args, input));
        }
    }

    #[cfg(windows)]
    fn fixture_config(root: &std::path::Path) -> Config {
        Config {
            home: root.to_string_lossy().into_owned(), dir: root.join("config"), state: root.join("state"),
            selves: vec!["example-local".into()], peers: vec!["example-peer".into()], base_peers: vec![],
            peers_from_environment: false, machines: std::collections::BTreeMap::new(), tab: false, tab_colors: String::new(),
        }
    }

    #[cfg(windows)]
    fn fixture_listener(name: &str) -> Arc<crate::host::pipe::PipeListener> {
        Arc::new(crate::host::pipe::PipeListener::bind(name, &crate::host::token::Identity::current().unwrap()).unwrap())
    }

    #[cfg(windows)]
    fn fixture_accept(listener: &Arc<crate::host::pipe::PipeListener>) -> crate::host::pipe::PipeStream {
        let owned = listener.clone();
        let (done, result) = mpsc::channel();
        let worker = thread::spawn(move || { let _ = done.send(owned.accept()); });
        let accepted = result.recv_timeout(Duration::from_secs(3));
        listener.close();
        worker.join().unwrap();
        accepted.expect("bounded fixture accept").unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn a_taken_request_obeys_its_deadline_and_never_becomes_a_fallback() {
        for mode in ["answer", "disconnect", "timeout"] {
            let root = tempfile::tempdir().unwrap();
            let config = fixture_config(root.path());
            let name = pipe_name(&config, "example-peer").unwrap();
            let listener = fixture_listener(&name);
            let server = thread::spawn(move || {
                let mut stream = fixture_accept(&listener);
                let request = read_frame(&mut stream).unwrap().unwrap();
                assert_eq!(request["args"], json!(["swarm", "hello"]));
                assert_eq!(bytes(&request["stdin"]), b"{}");
                if mode == "answer" {
                    write_frame(&mut stream, &json!({"code": 0, "stdout": STANDARD.encode(b"{}"), "stderr": ""})).unwrap();
                } else if mode == "timeout" {
                    // The client must close at its own deadline, waking this
                    // read without any answer or remote process cancellation.
                    assert_eq!(stream.read(&mut [0]).unwrap(), 0);
                }
            });
            let started = Instant::now();
            let answer = request_until(&config, "example-peer", &["swarm".into(), "hello".into()], Some(b"{}".to_vec()), started + Duration::from_millis(300));
            assert!(started.elapsed() < Duration::from_secs(2), "the 15 minute default must not replace the caller budget");
            let answer = answer.expect("a submitted request must never authorize fallback");
            if mode == "answer" {
                assert_eq!(answer.unwrap().stdout, b"{}");
            } else {
                assert!(answer.is_err());
            }
            server.join().unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "subprocess fixture for killed_client_releases_local_pending_and_active_slots"]
    fn pipe_client_fixture() {
        let name = std::env::var("HGS_PEER_TEST_PIPE").unwrap();
        let mut stream = crate::host::pipe::connect(&name, Instant::now() + Duration::from_secs(3)).unwrap();
        write_frame(&mut stream, &json!({"args": ["swarm", "hello"]})).unwrap();
        let _ = read_frame(&mut stream); // the parent kills only this fixture
    }

    #[cfg(windows)]
    #[test]
    fn killed_client_releases_local_pending_and_active_slots() {
        use std::os::windows::process::CommandExt;
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
        }
        struct Writer(Option<mpsc::Sender<()>>);
        impl Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { Ok(bytes.len()) }
            fn flush(&mut self) -> io::Result<()> {
                if let Some(sent) = self.0.take() { let _ = sent.send(()); }
                Ok(())
            }
        }
        for busy_writer in [false, true] {
            let name = format!(r"\\.\pipe\hgs-peer-test-{}", uuid::Uuid::new_v4());
            let listener = fixture_listener(&name);
            let mut child = Child(Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "peer_channel::tests::pipe_client_fixture", "--ignored"])
                .env("HGS_PEER_TEST_PIPE", &name)
                .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
            let mut stream = fixture_accept(&listener);
            let request = read_frame(&mut stream).unwrap().unwrap();
            let pending = Pending::default();
            let in_flight = InFlight::default();
            let active = Arc::new(AtomicUsize::new(1));
            let (sent, submitted) = mpsc::channel();
            let writer = Arc::new(Mutex::new(Writer(Some(sent))));
            // Also cover a request queued behind a blocked shared write.
            let held = busy_writer.then(|| writer.lock().unwrap());
            let (completed, settled) = mpsc::channel();
            let (own_pending, own_flight, own_active, own_writer) = (pending.clone(), in_flight.clone(), active.clone(), writer.clone());
            let worker = thread::spawn(move || {
                let result = relay_request(&mut stream, request, 1, &own_pending, own_flight, own_writer);
                own_active.fetch_sub(1, Ordering::SeqCst);
                let _ = completed.send(result);
            });
            if !busy_writer { submitted.recv_timeout(Duration::from_secs(3)).unwrap(); }
            child.0.kill().unwrap();
            child.0.wait().unwrap();
            settled.recv_timeout(Duration::from_secs(3)).expect("a killed client must free the broker slot before any remote answer").unwrap();
            assert!(pending.lock().unwrap().is_empty());
            assert_eq!(active.load(Ordering::SeqCst), 0);
            assert_eq!(in_flight.lock().unwrap().contains(&1), !busy_writer, "a submitted remote operation must outlive the local slot");
            drop(held);
            worker.join().unwrap();
        }
    }
}
