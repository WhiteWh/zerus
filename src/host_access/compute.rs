//! Local agent, remote compute (phase 6): complete file, process and result
//! mediation, so an agent whose sign-in stays on the workstation works on the
//! host's files without its native Read, Edit or Bash touching the wrong
//! computer.
//!
//! Host side, `hgs host-access compute --json`: one bounded operation inside a
//! root folder the caller owns. Paths are confined after resolving links;
//! commands run with a scrubbed environment that carries no provider or Zerus
//! authority, under the caller's own OS account. The host never starts an
//! agent here, so a workstation that is offline leaves nothing to launch or
//! resume: calls simply fail with a typed unreachable error.
//!
//! Workstation side, `hgs host-access remote-tools serve`: a stdio MCP server
//! that forwards each tool call through `hgs @host` (SSH as the person's own
//! account) and never falls back to local files or processes.
use super::gate::Refusal;
use super::providers::secret_env;
use super::runtime::{owned_by, Mode};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_REQUEST: usize = 2 * 1024 * 1024;
const MAX_READ: u64 = 1024 * 1024;
const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_LIST: usize = 1000;
const MAX_TIMEOUT: u64 = 600;
/// Environment kept for a mediated command; everything else is dropped.
const KEPT_ENV: &[&str] = &[
    "PATH", "PATHEXT", "SYSTEMROOT", "SystemRoot", "WINDIR", "COMSPEC", "TEMP", "TMP", "TMPDIR", "HOME", "USERPROFILE",
    "LANG", "LC_ALL", "TERM", "USER", "USERNAME", "LOGNAME", "SHELL",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, tag = "op", rename_all = "lowercase")]
pub enum Request {
    Read { root: String, path: String, #[serde(default)] offset: u64, #[serde(default)] limit: Option<u64> },
    Write { root: String, path: String, content: String },
    Edit { root: String, path: String, old: String, new: String, #[serde(default)] replace_all: bool },
    List { root: String, path: String },
    Exec { root: String, command: String, #[serde(default)] timeout_secs: Option<u64> },
}

fn refuse(code: &'static str, detail: &str) -> Refusal {
    Refusal::new(code, detail)
}

/// Provider homes whose presence means the context is not credential-free.
/// Only their existence is checked; nothing is read.
fn credential_homes(home: &Path) -> Vec<PathBuf> {
    [".claude/.credentials.json", ".codex/auth.json", ".kimi-code", ".qwen/oauth_creds.json", ".hermes"]
        .iter()
        .map(|relative| home.join(relative))
        .filter(|path| path.exists())
        .collect()
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// Resolves `root` for this caller.
fn admit_root(mode: &Mode, root: &str, state_root: &Path) -> Result<PathBuf, Refusal> {
    if let Mode::Broken(code) = mode {
        return Err(Refusal::new("policy-broken", format!("this host's access policy cannot be trusted ({code})")));
    }
    let root = Path::new(root);
    if !root.is_absolute() {
        return Err(refuse("root-invalid", "the compute root must be an absolute folder"));
    }
    let canonical = crate::platform::canonical(root).map_err(|_| refuse("root-invalid", "the compute root does not exist"))?;
    if !canonical.is_dir() {
        return Err(refuse("root-invalid", "the compute root is not a folder"));
    }
    let os = match mode {
        Mode::Bound { principal, .. } => Some(&principal.os),
        Mode::Unbound { os, .. } => Some(os),
        _ => None,
    };
    if let Some(os) = os {
        if !owned_by(&canonical, os) {
            return Err(refuse("root-owner", "the compute root belongs to another OS account"));
        }
    }
    // Never mediate into Zerus state or a provider home.
    let state = crate::platform::canonical(state_root).unwrap_or_else(|_| state_root.to_path_buf());
    if canonical.starts_with(&state) || state.starts_with(&canonical) {
        return Err(refuse("root-invalid", "the compute root may not contain or be inside Zerus state"));
    }
    if let Some(home) = home() {
        for name in [".claude", ".codex", ".kimi-code", ".qwen", ".hermes", ".ssh"] {
            let private = home.join(name);
            if canonical.starts_with(&private) || (private.exists() && private.starts_with(&canonical)) {
                return Err(refuse("root-invalid", "the compute root may not contain or be inside a provider or SSH home"));
            }
        }
        if matches!(mode, Mode::Bound { .. }) && !credential_homes(&home).is_empty() {
            return Err(refuse(
                "context-holds-credentials",
                "this execution context holds provider sign-ins; remote compute requires a credential-free context",
            ));
        }
    }
    Ok(canonical)
}

/// `path` inside `root`, following links of every existing ancestor.
fn confine(root: &Path, path: &str) -> Result<PathBuf, Refusal> {
    let outside = || refuse("path-outside", "the path is outside the compute root");
    let given = Path::new(path);
    let joined = if given.is_absolute() { given.to_path_buf() } else { root.join(given) };
    if joined.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(outside());
    }
    // The nearest existing ancestor decides; the rest must be plain names.
    let mut existing = joined.clone();
    let mut rest = Vec::new();
    while !existing.exists() {
        let name = existing.file_name().ok_or_else(outside)?.to_owned();
        rest.push(name);
        existing = existing.parent().ok_or_else(outside)?.to_path_buf();
    }
    let mut resolved = crate::platform::canonical(&existing).map_err(|_| outside())?;
    if !resolved.starts_with(root) {
        return Err(outside());
    }
    for name in rest.into_iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

fn read_capped(path: &Path, offset: u64, limit: u64) -> Result<(String, u64), Refusal> {
    use std::io::Seek;
    let mut file = std::fs::File::open(path).map_err(|_| refuse("not-found", "the file cannot be opened"))?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    file.seek(std::io::SeekFrom::Start(offset)).map_err(|_| refuse("io", "the file cannot be read"))?;
    let mut bytes = Vec::new();
    file.take(limit.min(MAX_READ)).read_to_end(&mut bytes).map_err(|_| refuse("io", "the file cannot be read"))?;
    Ok((String::from_utf8_lossy(&bytes).into_owned(), size))
}

fn write_file(path: &Path, content: &[u8]) -> Result<(), Refusal> {
    let dir = path.parent().ok_or_else(|| refuse("path-outside", "no parent folder"))?;
    std::fs::create_dir_all(dir).map_err(|_| refuse("io", "the folder cannot be created"))?;
    let temp = dir.join(format!(".zerus-{}.tmp", uuid::Uuid::new_v4()));
    let result = std::fs::write(&temp, content).and_then(|()| std::fs::rename(&temp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(|_| refuse("io", "the file cannot be written"))
}

/// The scrubbed environment of a mediated command.
pub fn mediated_env() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(name, _)| KEPT_ENV.contains(&name.as_str()) && !secret_env(name))
        .chain(std::iter::once(("ZERUS_REMOTE_COMPUTE".to_owned(), "1".to_owned())))
        .collect()
}

fn run_command(root: &Path, command: &str, timeout: Duration) -> Result<Value, Refusal> {
    use std::process::{Command, Stdio};
    let mut process = if cfg!(windows) {
        let mut process = Command::new("cmd.exe");
        process.args(["/D", "/S", "/C", command]);
        process
    } else {
        let mut process = Command::new("/bin/sh");
        process.args(["-c", command]);
        process
    };
    process.current_dir(root).env_clear().envs(mediated_env()).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = process.spawn().map_err(|_| refuse("spawn", "the command could not be started"))?;
    let collect = |mut stream: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            let mut buffer = [0u8; 8192];
            let mut truncated = false;
            while let Ok(n) = stream.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                let room = MAX_OUTPUT.saturating_sub(kept.len());
                kept.extend_from_slice(&buffer[..n.min(room)]);
                truncated |= n > room;
            }
            (kept, truncated)
        })
    };
    let out = collect(Box::new(child.stdout.take().expect("piped")));
    let err = collect(Box::new(child.stderr.take().expect("piped")));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break None,
        }
    };
    let (stdout, out_cut) = out.join().unwrap_or_default();
    let (stderr, err_cut) = err.join().unwrap_or_default();
    Ok(json!({
        "exit_code":status.and_then(|s| s.code()),
        "timed_out":status.is_none(),
        "stdout":String::from_utf8_lossy(&stdout),
        "stderr":String::from_utf8_lossy(&stderr),
        "truncated":out_cut || err_cut,
    }))
}

/// Serves one host-side request.
pub fn serve(mode: &Mode, request: Request, state_root: &Path) -> Result<Value, Refusal> {
    match request {
        Request::Read { root, path, offset, limit } => {
            let root = admit_root(mode, &root, state_root)?;
            let path = confine(&root, &path)?;
            let (content, size) = read_capped(&path, offset, limit.unwrap_or(MAX_READ))?;
            Ok(json!({"content":content,"size":size,"offset":offset}))
        }
        Request::Write { root, path, content } => {
            let root = admit_root(mode, &root, state_root)?;
            let path = confine(&root, &path)?;
            write_file(&path, content.as_bytes())?;
            Ok(json!({"written":content.len()}))
        }
        Request::Edit { root, path, old, new, replace_all } => {
            let root = admit_root(mode, &root, state_root)?;
            let path = confine(&root, &path)?;
            if old.is_empty() || old == new {
                return Err(refuse("edit-invalid", "old must be non-empty and differ from new"));
            }
            let meta = std::fs::metadata(&path).map_err(|_| refuse("not-found", "the file cannot be opened"))?;
            if meta.len() > MAX_READ * 4 {
                return Err(refuse("too-large", "the file is too large to edit remotely"));
            }
            let text = std::fs::read_to_string(&path).map_err(|_| refuse("io", "the file is not UTF-8 text"))?;
            let count = text.matches(&old).count();
            if count == 0 {
                return Err(refuse("edit-not-found", "old text was not found"));
            }
            if count > 1 && !replace_all {
                return Err(refuse("edit-ambiguous", "old text occurs more than once; pass replace_all"));
            }
            let updated = if replace_all { text.replace(&old, &new) } else { text.replacen(&old, &new, 1) };
            write_file(&path, updated.as_bytes())?;
            Ok(json!({"replaced":if replace_all { count } else { 1 }}))
        }
        Request::List { root, path } => {
            let root = admit_root(mode, &root, state_root)?;
            let path = confine(&root, &path)?;
            let mut entries = Vec::new();
            let mut truncated = false;
            for entry in std::fs::read_dir(&path).map_err(|_| refuse("not-found", "the folder cannot be listed"))?.flatten() {
                if entries.len() >= MAX_LIST {
                    truncated = true;
                    break;
                }
                let kind = entry.file_type().map(|t| if t.is_dir() { "dir" } else if t.is_symlink() { "link" } else { "file" }).unwrap_or("unknown");
                entries.push(json!({"name":entry.file_name().to_string_lossy(),"kind":kind}));
            }
            Ok(json!({"entries":entries,"truncated":truncated}))
        }
        Request::Exec { root, command, timeout_secs } => {
            let root = admit_root(mode, &root, state_root)?;
            if command.is_empty() || command.len() > 64 * 1024 {
                return Err(refuse("command-invalid", "the command must be 1 byte to 64 KiB"));
            }
            run_command(&root, &command, Duration::from_secs(timeout_secs.unwrap_or(120).clamp(1, MAX_TIMEOUT)))
        }
    }
}

/// Reads and serves `hgs host-access compute --json` from stdin.
pub fn serve_stdin(mode: &Mode, state_root: &Path) -> Result<Value, Refusal> {
    let mut bytes = Vec::new();
    std::io::stdin().take(MAX_REQUEST as u64 + 1).read_to_end(&mut bytes).map_err(|_| refuse("request-invalid", "stdin unreadable"))?;
    if bytes.len() > MAX_REQUEST {
        return Err(refuse("too-large", "the request exceeds 2 MiB"));
    }
    let request: Request =
        serde_json::from_slice(crate::state::without_bom(&bytes)).map_err(|_| refuse("request-invalid", "malformed compute request"))?;
    serve(mode, request, state_root)
}

// ---------------------------------------------------------------- workstation

/// The argv additions that start Claude Code with only Zerus's remote tools:
/// built-in tools off and no MCP servers but this one. The configuration
/// names a host alias and a folder; it carries no token.
pub fn claude_args(hgs: &Path, host: &str, root: &str) -> Result<Vec<String>, Refusal> {
    validate_host(host)?;
    if !Path::new(root).is_absolute() && !root.starts_with('/') {
        return Err(refuse("root-invalid", "the remote root must be an absolute folder"));
    }
    let config = json!({"mcpServers":{"zerus-remote":{
        "command":hgs,
        "args":["host-access","remote-tools","serve","--host",host,"--root",root],
    }}});
    Ok(vec!["--tools".into(), String::new(), "--strict-mcp-config".into(), "--mcp-config".into(), config.to_string()])
}

fn validate_host(host: &str) -> Result<(), Refusal> {
    let ok = !host.is_empty() && host.len() <= 128 && !host.starts_with('-')
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if ok { Ok(()) } else { Err(refuse("host-invalid", "name a configured host alias")) }
}

fn tool_list() -> Value {
    let text = |description: &str| json!({"type":"string","description":description});
    json!([
        {"name":"remote_read","description":"Read a text file on the remote host (bounded to 1 MiB per call).",
         "inputSchema":{"type":"object","properties":{"path":text("Path inside the remote root"),"offset":{"type":"integer"},"limit":{"type":"integer"}},"required":["path"]}},
        {"name":"remote_write","description":"Create or replace a file on the remote host.",
         "inputSchema":{"type":"object","properties":{"path":text("Path inside the remote root"),"content":text("Full file content")},"required":["path","content"]}},
        {"name":"remote_edit","description":"Replace exact text in a remote file.",
         "inputSchema":{"type":"object","properties":{"path":text("Path inside the remote root"),"old":text("Exact text to replace"),"new":text("Replacement"),"replace_all":{"type":"boolean"}},"required":["path","old","new"]}},
        {"name":"remote_list","description":"List a folder on the remote host.",
         "inputSchema":{"type":"object","properties":{"path":text("Folder inside the remote root")},"required":["path"]}},
        {"name":"remote_bash","description":"Run a shell command in the remote root, credential-free, with a timeout.",
         "inputSchema":{"type":"object","properties":{"command":text("Shell command"),"timeout_secs":{"type":"integer"}},"required":["command"]}},
    ])
}

/// Builds the host-side request of one tool call. Unknown tools and argument
/// names are refused, so a call can never widen what is mediated.
pub fn tool_request(root: &str, name: &str, arguments: &Value) -> Result<Value, Refusal> {
    let object = arguments.as_object().ok_or_else(|| refuse("arguments-invalid", "arguments must be an object"))?;
    let allowed: &[&str] = match name {
        "remote_read" => &["path", "offset", "limit"],
        "remote_write" => &["path", "content"],
        "remote_edit" => &["path", "old", "new", "replace_all"],
        "remote_list" => &["path"],
        "remote_bash" => &["command", "timeout_secs"],
        _ => return Err(refuse("tool-unknown", "no such remote tool")),
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(refuse("arguments-invalid", "unexpected argument"));
    }
    let op = &name["remote_".len()..];
    let mut request = arguments.clone();
    request["op"] = json!(if op == "bash" { "exec" } else { op });
    request["root"] = json!(root);
    Ok(request)
}

/// Forwards one request to the host through this hgs. Exit 3 is the CLI's
/// unreachable code; any failure is reported, never retried locally.
fn forward(hgs: &Path, host: &str, request: &Value) -> Result<Value, String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(hgs)
        .args([format!("@{host}"), "host-access".into(), "compute".into(), "--json".into()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "host-unreachable: hgs could not be started".to_owned())?;
    child.stdin.take().expect("piped").write_all(request.to_string().as_bytes()).map_err(|_| "host-unreachable".to_owned())?;
    let output = child.wait_with_output().map_err(|_| "host-unreachable".to_owned())?;
    if output.status.code() == Some(3) {
        return Err("host-unreachable: the remote host is offline; nothing ran locally".into());
    }
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(4096)]).trim().to_owned();
        return Err(if message.is_empty() { "remote tool failed".into() } else { message });
    }
    serde_json::from_slice(&output.stdout).map_err(|_| "remote tool returned malformed output".into())
}

/// One MCP JSON-RPC message; None for a notification.
pub fn handle(message: &Value, root: &str, call: &mut dyn FnMut(&Value) -> Result<Value, String>) -> Option<Value> {
    let id = message.get("id")?.clone();
    let method = message["method"].as_str().unwrap_or("");
    let result = match method {
        "initialize" => json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},
            "serverInfo":{"name":"zerus-remote","version":env!("CARGO_PKG_VERSION")}}),
        "ping" => json!({}),
        "tools/list" => json!({"tools":tool_list()}),
        "tools/call" => {
            let name = message["params"]["name"].as_str().unwrap_or("");
            let arguments = message["params"].get("arguments").cloned().unwrap_or_else(|| json!({}));
            let outcome = tool_request(root, name, &arguments).map_err(|r| r.message()).and_then(|request| call(&request));
            match outcome {
                Ok(value) => json!({"content":[{"type":"text","text":value.to_string()}],"isError":false}),
                Err(error) => json!({"content":[{"type":"text","text":error}],"isError":true}),
            }
        }
        _ => return Some(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not found"}})),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

/// The stdio MCP server loop.
pub fn serve_mcp(hgs: &Path, host: &str, root: &str) -> Result<(), Refusal> {
    use std::io::BufRead;
    validate_host(host)?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut call = |request: &Value| forward(hgs, host, request);
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() || line.len() > MAX_REQUEST {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
        if let Some(reply) = handle(&message, root, &mut call) {
            let _ = writeln!(stdout, "{reply}");
            let _ = stdout.flush();
        }
    }
    Ok(())
}
