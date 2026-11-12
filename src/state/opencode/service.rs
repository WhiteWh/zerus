//! OpenCode 2's shared per-user background service: discovery through its
//! registration file, the version and API gate, and authenticated loopback
//! HTTP. Zerus never stops, restarts or replaces this service.
use super::super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) const MINIMUM: (u64, u64, u64) = (2, 0, 32);

/// Method (lowercase), path template, request fields the adapter sends.
pub(super) const REQUIRED: &[(&str, &str, &[&str])] = &[
    ("get", "/api/info", &[]),
    ("get", "/api/session/active", &[]),
    (
        "post",
        "/api/session",
        &["id", "location", "model", "title", "metadata"],
    ),
    ("get", "/api/session/{sessionID}", &[]),
    ("patch", "/api/session/{sessionID}", &["title"]),
    (
        "post",
        "/api/session/{sessionID}/prompt",
        &["id", "text", "delivery"],
    ),
    ("get", "/api/session/{sessionID}/message", &[]),
    ("get", "/api/session/{sessionID}/message/{messageID}", &[]),
    ("get", "/api/session/{sessionID}/inbox", &[]),
    ("delete", "/api/session/{sessionID}/inbox/{inboxID}", &[]),
    ("post", "/api/session/{sessionID}/interrupt", &[]),
    ("post", "/api/session/{sessionID}/model", &["model"]),
    ("get", "/api/session/{sessionID}/form", &[]),
    (
        "post",
        "/api/session/{sessionID}/form/{formID}/reply",
        &["answer"],
    ),
    ("get", "/api/session/{sessionID}/permission", &[]),
    (
        "post",
        "/api/session/{sessionID}/permission/{requestID}/reply",
        &["decision"],
    ),
    ("get", "/api/model", &[]),
    ("get", "/api/model/default", &[]),
    ("get", "/api/provider", &[]),
    ("get", "/api/experimental/migration/v1", &[]),
];

pub(super) struct Registration {
    pub id: String,
    pub version: String,
    pub port: u16,
    pub pid: u32,
    pub password: Option<String>,
}

pub(super) fn registration_path() -> PathBuf {
    registration_path_from(nonempty_env("XDG_STATE_HOME").as_deref(), &home())
}

pub(super) fn registration_path_from(state_home: Option<&str>, home: &Path) -> PathBuf {
    state_home
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state"))
        .join("opencode/service.json")
}

pub(super) fn parse_registration(bytes: &[u8]) -> Result<Registration> {
    if bytes.len() > 16384 {
        return Err("Invalid OpenCode service registration".into());
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "Invalid OpenCode service registration")?;
    let text = |key: &str, max: usize| {
        value[key]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
    };
    let (Some(id), Some(version), Some(url)) =
        (text("id", 128), text("version", 64), text("url", 64))
    else {
        return Err("Invalid OpenCode service registration".into());
    };
    let rest = url
        .strip_prefix("http://127.0.0.1:")
        .or_else(|| url.strip_prefix("http://localhost:"));
    let port = rest
        .map(|r| r.trim_end_matches('/'))
        .filter(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|r| r.parse::<u16>().ok())
        .filter(|p| *p > 0)
        .ok_or("OpenCode service is not on loopback")?;
    let pid = value["pid"]
        .as_u64()
        .filter(|p| *p > 1 && *p <= u32::MAX as u64)
        .ok_or("Invalid OpenCode service registration")? as u32;
    let password = match &value["password"] {
        Value::Null => None,
        Value::String(p) if !p.is_empty() && p.len() <= 512 => Some(p.clone()),
        _ => return Err("Invalid OpenCode service registration".into()),
    };
    Ok(Registration {
        id: id.into(),
        version: version.into(),
        port,
        pid,
        password,
    })
}

pub(super) fn version_at_least(text: &str, minimum: (u64, u64, u64)) -> bool {
    let core = text.trim().trim_start_matches('v');
    if core.contains(['-', '+']) {
        return false;
    }
    let parts: Vec<u64> = core
        .split('.')
        .map(|p| p.parse::<u64>())
        .collect::<std::result::Result<_, _>>()
        .unwrap_or_default();
    parts.len() == 3 && parts[0] == minimum.0 && (parts[1], parts[2]) >= (minimum.1, minimum.2)
}

pub(super) fn missing_operations(openapi: &Value) -> Vec<String> {
    let mut missing = Vec::new();
    for (method, path, fields) in REQUIRED {
        let operation = &openapi["paths"][*path][*method];
        if !operation.is_object() {
            missing.push(format!("{} {path}", method.to_uppercase()));
            continue;
        }
        let schema = &operation["requestBody"]["content"]["application/json"]["schema"];
        for field in *fields {
            if !schema_has_field(openapi, schema, field, 8) {
                missing.push(format!("{} {path} field {field}", method.to_uppercase()));
            }
        }
    }
    missing
}

/// Whether a request schema offers `field`: inline properties, local
/// `#/components/schemas/<Name>` references (bounded depth, so cycles end),
/// `allOf` merged, and any `anyOf`/`oneOf` variant.
fn schema_has_field(openapi: &Value, schema: &Value, field: &str, depth: u8) -> bool {
    if depth == 0 || !schema.is_object() {
        return false;
    }
    if let Some(reference) = schema["$ref"].as_str() {
        let Some(name) = reference.strip_prefix("#/components/schemas/") else {
            return false;
        };
        return schema_has_field(
            openapi,
            &openapi["components"]["schemas"][name],
            field,
            depth - 1,
        );
    }
    schema["properties"].get(field).is_some()
        || ["allOf", "anyOf", "oneOf"].iter().any(|key| {
            schema[*key].as_array().is_some_and(|variants| {
                variants
                    .iter()
                    .any(|v| schema_has_field(openapi, v, field, depth - 1))
            })
        })
}

#[derive(Clone)]
pub(super) struct Response {
    pub status: u16,
    pub body: Value,
}

struct Frame {
    header_end: usize,
    end: usize,
    status: u16,
    chunked: bool,
}
fn frame(bytes: &[u8]) -> Result<Option<Frame>> {
    let Some(split) = bytes.windows(4).position(|p| p == b"\r\n\r\n") else {
        if bytes.len() > 32768 {
            return Err("Native HTTP headers exceed bounds".into());
        }
        return Ok(None);
    };
    if split > 32768 {
        return Err("Native HTTP headers exceed bounds".into());
    }
    let header = std::str::from_utf8(&bytes[..split]).map_err(|_| "Invalid native HTTP headers")?;
    let mut lines = header.split("\r\n");
    let mut status_line = lines
        .next()
        .ok_or("Missing native HTTP status")?
        .split_whitespace();
    if !matches!(status_line.next(), Some("HTTP/1.1" | "HTTP/1.0")) {
        return Err("Invalid native HTTP protocol".into());
    }
    let status = status_line
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|s| (100..600).contains(s))
        .ok_or("Invalid native HTTP status")?;
    let mut length = None;
    let mut chunked = false;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or("Invalid native HTTP header")?;
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err("Duplicate native Content-Length".into());
            }
            let n = value.trim();
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return Err("Invalid native Content-Length".into());
            }
            length = Some(
                n.parse::<usize>()
                    .map_err(|_| "Native Content-Length exceeds bounds")?,
            );
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.trim().eq_ignore_ascii_case("chunked") {
                return Err("Unsupported native Transfer-Encoding".into());
            }
            chunked = true;
        }
    }
    if chunked && length.is_some() {
        return Err("Conflicting native HTTP framing".into());
    }
    let body_start = split + 4;
    let end = if chunked {
        let mut offset = body_start;
        loop {
            let rest = &bytes[offset..];
            let Some(n) = rest.windows(2).position(|p| p == b"\r\n") else {
                return Ok(None);
            };
            if n > 1024 {
                return Err("Native HTTP chunk header exceeds bounds".into());
            }
            let raw = std::str::from_utf8(&rest[..n]).map_err(|_| "Invalid native chunk size")?;
            let size_text = raw.split(';').next().unwrap_or("");
            if size_text.is_empty() || !size_text.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("Invalid native chunk size".into());
            }
            let size = usize::from_str_radix(size_text, 16)
                .map_err(|_| "Native chunk size exceeds bounds")?;
            offset += n + 2;
            if size == 0 {
                let trailers = &bytes[offset..];
                if trailers.starts_with(b"\r\n") {
                    break offset + 2;
                }
                let Some(trailer_end) = trailers.windows(4).position(|p| p == b"\r\n\r\n") else {
                    return Ok(None);
                };
                if trailer_end > 32768 {
                    return Err("Native HTTP trailers exceed bounds".into());
                }
                let trailer = std::str::from_utf8(&trailers[..trailer_end])
                    .map_err(|_| "Invalid native HTTP trailer")?;
                if trailer.split("\r\n").any(|line| !line.contains(':')) {
                    return Err("Invalid native HTTP trailer".into());
                }
                break offset + trailer_end + 4;
            }
            let next = offset
                .checked_add(size)
                .and_then(|n| n.checked_add(2))
                .ok_or("Native chunk size exceeds bounds")?;
            if next > 32 * 1024 * 1024 {
                return Err("Native chunk body exceeds bounds".into());
            }
            if next > bytes.len() {
                return Ok(None);
            }
            if &bytes[next - 2..next] != b"\r\n" {
                return Err("Invalid native chunk boundary".into());
            }
            offset = next;
        }
    } else if let Some(length) = length {
        let end = body_start
            .checked_add(length)
            .filter(|n| *n <= 32 * 1024 * 1024)
            .ok_or("Native HTTP body exceeds bounds")?;
        if bytes.len() < end {
            return Ok(None);
        }
        end
    } else if status == 204 {
        body_start
    } else {
        return Err("Missing native HTTP body framing".into());
    };
    Ok(Some(Frame {
        header_end: body_start,
        end,
        status,
        chunked,
    }))
}
pub(super) fn decode_response(bytes: &[u8]) -> Result<Response> {
    let framing = frame(bytes)?.ok_or("Incomplete native HTTP response; delivery uncertain")?;
    if framing.end != bytes.len() {
        return Err("Unexpected bytes after native HTTP response".into());
    }
    let raw = &bytes[framing.header_end..framing.end];
    let mut decoded = Vec::new();
    let body = if framing.chunked {
        let mut rest = raw;
        loop {
            let n = rest
                .windows(2)
                .position(|p| p == b"\r\n")
                .ok_or("Invalid native chunk")?;
            let size = usize::from_str_radix(
                std::str::from_utf8(&rest[..n])
                    .map_err(|_| "Invalid native chunk")?
                    .split(';')
                    .next()
                    .unwrap_or(""),
                16,
            )
            .map_err(|_| "Invalid native chunk")?;
            rest = &rest[n + 2..];
            if size == 0 {
                break;
            }
            decoded.extend_from_slice(&rest[..size]);
            rest = &rest[size + 2..];
        }
        decoded.as_slice()
    } else {
        raw
    };
    let value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(body).unwrap_or_else(|_| {
            Value::String(String::from_utf8_lossy(&body[..body.len().min(4096)]).into_owned())
        })
    };
    Ok(Response {
        status: framing.status,
        body: value,
    })
}

/// A registered service that does not answer. It is never restarted, and an
/// OpenCode 2 session has no Resume that could reconnect it.
const NOT_RESPONDING: &str = "OpenCode is not responding";

pub(super) fn request(
    reg: &Registration,
    method: &str,
    path: &str,
    body: Option<&Value>,
    deadline: Instant,
) -> Result<Response> {
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], reg.port));
    let mut socket = TcpStream::connect_timeout(
        &address,
        deadline
            .checked_duration_since(Instant::now())
            .ok_or("Native HTTP deadline exceeded")?
            .min(Duration::from_secs(2)),
    )
    .map_err(|_| NOT_RESPONDING.to_owned())?;
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    socket
        .set_write_timeout(Some(
            deadline
                .checked_duration_since(Instant::now())
                .ok_or("Native HTTP deadline exceeded")?
                .min(Duration::from_secs(5)),
        ))
        .map_err(|e| e.to_string())?;
    let body = body.map(Value::to_string).unwrap_or_default();
    let auth = reg
        .password
        .as_ref()
        .map(|p| {
            format!(
                "Authorization: Basic {}\r\n",
                STANDARD.encode(format!("opencode:{p}"))
            )
        })
        .unwrap_or_default();
    let headers = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket
        .write_all(headers.as_bytes())
        .and_then(|_| socket.write_all(body.as_bytes()))
        .map_err(|e| format!("Native request delivery uncertain: {e}"))?;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or("Native HTTP deadline exceeded; delivery uncertain")?;
        socket
            .set_read_timeout(Some(remaining))
            .map_err(|e| e.to_string())?;
        let n = socket
            .read(&mut chunk)
            .map_err(|e| format!("Native response delivery uncertain: {e}"))?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if bytes.len() > 32 * 1024 * 1024 {
            return Err("Native response exceeds bounds; delivery uncertain".into());
        }
        if frame(&bytes)?.is_some() {
            break;
        }
    }
    decode_response(&bytes)
}

pub(super) struct Service {
    pub reg: Registration,
}

impl Service {
    pub(super) fn raw(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Response> {
        request(
            &self.reg,
            method,
            path,
            body,
            Instant::now() + Duration::from_secs(10),
        )
    }

    /// A read with its own deadline, for reads that must not hold up a listing.
    /// A refusal names only the status: catalog reads never repeat what the
    /// service answered.
    pub(super) fn call_within(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        limit: Duration,
    ) -> Result<Value> {
        let response = request(&self.reg, method, path, body, Instant::now() + limit)?;
        if (200..300).contains(&response.status) {
            return Ok(response.body);
        }
        Err(format!("OpenCode HTTP {}", response.status))
    }
}

pub(super) enum Unavailable {
    NotRunning,
    Disabled,
    Incompatible(String),
    Unreachable(String),
}

/// This computer as hgs names itself in listings (`host`): its first
/// `HGS_SELF` name, by default its host name.
pub(super) fn computer() -> String {
    crate::config::Config::load()
        .map(|config| config.host().to_owned())
        .unwrap_or_else(|_| crate::platform::hostname())
}

impl Unavailable {
    pub(super) fn reason(&self, computer: &str) -> String {
        match self {
            Unavailable::NotRunning => format!("OpenCode is not running on {computer}"),
            Unavailable::Disabled => {
                format!("The OpenCode background service is disabled on {computer}")
            }
            Unavailable::Incompatible(why) | Unavailable::Unreachable(why) => why.clone(),
        }
    }
}

const UNREADABLE_REGISTRATION: &str = "The OpenCode service registration cannot be read";

/// The registration file as read now. Only a file that does not exist is
/// `Missing`; any other open or read failure, an oversized file or one that
/// does not parse is `Unreadable`, never "no service".
pub(super) enum RegistrationRead {
    Missing,
    Unreadable(String),
    Present(Registration),
}

fn read_registration() -> RegistrationRead {
    read_registration_at(&registration_path())
}

/// Read with the same bound the parser enforces.
pub(super) fn read_registration_at(path: &Path) -> RegistrationRead {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return RegistrationRead::Missing,
        Err(_) => return RegistrationRead::Unreadable(UNREADABLE_REGISTRATION.into()),
    };
    let mut bytes = Vec::new();
    if file.take(16385).read_to_end(&mut bytes).is_err() {
        return RegistrationRead::Unreadable(UNREADABLE_REGISTRATION.into());
    }
    match parse_registration(&bytes) {
        Ok(reg) => RegistrationRead::Present(reg),
        Err(why) => RegistrationRead::Unreadable(why),
    }
}

pub(super) fn connect() -> std::result::Result<Service, Unavailable> {
    connect_from(read_registration())
}

fn connect_from(read: RegistrationRead) -> std::result::Result<Service, Unavailable> {
    let reg = match read {
        RegistrationRead::Missing => {
            return Err(if service_disabled() {
                Unavailable::Disabled
            } else {
                Unavailable::NotRunning
            })
        }
        RegistrationRead::Unreadable(why) => return Err(Unavailable::Unreachable(why)),
        RegistrationRead::Present(reg) => reg,
    };
    if !crate::platform::process_alive(reg.pid) {
        return Err(Unavailable::NotRunning);
    }
    let info = request(
        &reg,
        "GET",
        "/api/info",
        None,
        Instant::now() + Duration::from_secs(2),
    )
    .map_err(|_| Unavailable::Unreachable(NOT_RESPONDING.into()))?;
    if info.status != 200 {
        return Err(Unavailable::Unreachable(
            "OpenCode is not responding correctly".into(),
        ));
    }
    let version = clipped_version(info.body["version"].as_str().unwrap_or(""));
    let version = version.as_str();
    if !version_at_least(version, MINIMUM) {
        return Err(Unavailable::Incompatible(format!(
            "OpenCode {version} is not supported; install OpenCode {}.{}.{} or newer 2.x",
            MINIMUM.0, MINIMUM.1, MINIMUM.2
        )));
    }
    let service = Service { reg };
    let missing = compatibility(&service).map_err(Unavailable::Unreachable)?;
    if let Some(first) = missing.first() {
        return Err(Unavailable::Incompatible(format!(
            "OpenCode {version} lacks {first}; update OpenCode or Zerus"
        )));
    }
    Ok(service)
}

/// Digest of the requirement table, so a Zerus with new requirements
/// re-checks a service whose verdict was cached by an older one.
fn requirement_digest() -> String {
    format!(
        "{:x}",
        Sha256::digest(json!(REQUIRED).to_string().as_bytes())
    )
}

fn verdict(openapi: &Value) -> Result<Vec<String>> {
    if !openapi["paths"].is_object() {
        return Err("OpenCode returned an invalid API description".into());
    }
    Ok(missing_operations(openapi))
}

/// The OpenAPI verdict, cached per service identity and requirement digest in
/// private state. The cache write is best effort.
fn compatibility(service: &Service) -> Result<Vec<String>> {
    compatibility_in(
        &root().join("opencode/compat"),
        &service.reg,
        &requirement_digest(),
        || fetch_description(service),
    )
}

/// The API description. A refusal names only its status: the service's own
/// text must not reach `catalog_reason`, which every account listing sends on.
pub(super) fn fetch_description(service: &Service) -> Result<Value> {
    service.call_within("GET", "/openapi.json", None, Duration::from_secs(10))
}

pub(super) fn compatibility_in(
    dir: &Path,
    reg: &Registration,
    digest: &str,
    fetch: impl FnOnce() -> Result<Value>,
) -> Result<Vec<String>> {
    let key = format!(
        "{:x}",
        Sha256::digest(
            json!([reg.id, reg.pid, reg.version, reg.port, digest])
                .to_string()
                .as_bytes()
        )
    );
    let cache = dir.join(format!("{key}.json"));
    if let Ok(bytes) = fs::read(&cache) {
        if let Ok(value) = serde_json::from_slice::<Vec<String>>(&bytes) {
            return Ok(value);
        }
    }
    let missing = verdict(&fetch()?)?;
    let _ = atomic(&cache, &json!(missing).to_string());
    Ok(missing)
}

fn clipped_version(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(64).collect()
}

/// OpenCode 2.0.32 keeps `disabled` in the service configuration file
/// `<config dir>/service.json` (the channel filename for the stable channel;
/// upstream `packages/cli/src/services/service-config.ts`: `filename()`,
/// `paths` and `get`/`set` for the `disabled` key).
fn service_disabled() -> bool {
    let base = nonempty_env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"));
    fs::read(base.join("opencode/service.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .is_some_and(|v| v["disabled"] == true)
}

pub(super) enum StartDecision {
    AlreadyRunning,
    Start,
    Refuse(String),
}

/// Start runs only when no registration file exists or its process is gone;
/// it never runs while a registered process lives, however slow, nor when
/// the registration cannot be read.
pub(super) fn start_decision(
    registration: &RegistrationRead,
    pid_alive: bool,
    disabled: bool,
) -> StartDecision {
    match registration {
        RegistrationRead::Present(_) if pid_alive => return StartDecision::AlreadyRunning,
        RegistrationRead::Unreadable(why) => return StartDecision::Refuse(why.clone()),
        _ => {}
    }
    if disabled {
        return StartDecision::Refuse(
            "The OpenCode background service is disabled; start it in OpenCode".into(),
        );
    }
    StartDecision::Start
}

/// Everything `ensure_started` decides before it may run `opencode service
/// start`: a connected service is used, any unavailability other than "not
/// running" or "disabled" is refused, and otherwise the registration is read
/// again for the start decision. `Ok(None)` means start.
fn may_start(
    connected: std::result::Result<Service, Unavailable>,
    read: impl FnOnce() -> RegistrationRead,
    alive: impl FnOnce(u32) -> bool,
    disabled: impl FnOnce() -> bool,
) -> Result<Option<Service>> {
    match connected {
        Ok(service) => return Ok(Some(service)),
        Err(Unavailable::NotRunning | Unavailable::Disabled) => {}
        // Only not running and disabled name the computer.
        Err(other) => return Err(other.reason("")),
    }
    let read = read();
    let alive = matches!(&read, RegistrationRead::Present(r) if alive(r.pid));
    match start_decision(&read, alive, disabled()) {
        StartDecision::AlreadyRunning => Err("OpenCode is starting; try again shortly".into()),
        StartDecision::Refuse(why) => Err(why),
        StartDecision::Start => Ok(None),
    }
}

/// Variables the shared service must not inherit from `hgs`, the connector
/// or an SSH login: `HGS_*`, `SSH_*`, `OPENCODE_SERVER_*`, `ZERUS_MOBILE_*`,
/// and the inline configuration and permission overrides
/// `OPENCODE_CONFIG_CONTENT` and `OPENCODE_PERMISSION`. `OPENCODE_CONFIG`,
/// `OPENCODE_CONFIG_DIR`, `HOME` and every `XDG_*` variable stay: they name
/// the user's own configuration and state locations, the user's own
/// `opencode` CLI would pass them too, and the service must register its
/// `service.json` where `registration_path()` looks.
/// Only HGS_STATE_DIR/HGS_CONFIG_DIR survive from HGS_*: they let a native
/// tool shell address its Zerus catalog, and never assert a session identity.
pub(super) fn start_environment(vars: Vec<(String, String)>) -> Vec<(String, String)> {
    vars.into_iter()
        .filter(|(k, _)| {
            !((k.starts_with("HGS_") && !["HGS_STATE_DIR", "HGS_CONFIG_DIR"].contains(&k.as_str()))
                || k.starts_with("SSH_")
                || k.starts_with("OPENCODE_SERVER_")
                || k.starts_with("ZERUS_MOBILE_")
                || k == "OPENCODE_CONFIG_CONTENT"
                || k == "OPENCODE_PERMISSION")
        })
        .collect()
}

/// Collect the child's exit without blocking, so it never stays a zombie.
fn reap(mut child: std::process::Child) {
    if !matches!(child.try_wait(), Ok(Some(_))) {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Connect, starting `opencode service start` only when no service runs.
/// A live registered process that does not answer is reported, never replaced.
#[allow(dead_code)]
pub(super) fn ensure_started() -> Result<Service> {
    let lock_path = root().join("opencode/service-start.lock");
    let _guard = lock(Some(&lock_path))?;
    if let Some(service) = may_start(
        connect(),
        read_registration,
        crate::platform::process_alive,
        service_disabled,
    )? {
        return Ok(service);
    }
    // Optional launch-only overlay: never edit the user's config or add a
    // plugin to a service already running. Inline caller overrides stay dropped.
    let overlay = super::context_hint::overlay().filter(super::context_hint::starting);
    let mut command = Command::new("opencode");
    #[cfg(target_os = "linux")]
    let mut runtime_dir = None;
    #[cfg(target_os = "linux")]
    {
        let runtime = PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }));
        if runtime.join("systemd/private").exists() && crate::platform::available("systemd-run") {
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
                .arg(format!("hgs-opencode-{}", uuid::Uuid::new_v4()))
                .args(["--", "opencode"]);
            runtime_dir = Some(runtime);
        }
    }
    command
        .args(["service", "start"])
        .env_clear()
        .envs(start_environment(std::env::vars().collect()))
        .current_dir(home())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(overlay) = &overlay {
        command.env("OPENCODE_CONFIG_CONTENT", &overlay.content);
    }
    #[cfg(target_os = "linux")]
    if let Some(runtime) = runtime_dir {
        command.env("XDG_RUNTIME_DIR", runtime);
    }
    let mut child = crate::platform::spawn_detached(&mut command).map_err(|_| {
        if let Some(overlay) = &overlay { super::context_hint::spawn_failed(overlay); }
        "OpenCode could not be started; install OpenCode 2".to_owned()
    })?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut failed = false;
    while Instant::now() < deadline {
        if let Ok(service) = connect() {
            if let Some(overlay) = &overlay {
                super::context_hint::started(&service, overlay);
            }
            reap(child);
            return Ok(service);
        }
        if let Ok(Some(status)) = child.try_wait() {
            if !status.success() {
                failed = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if let Some(overlay) = &overlay { super::context_hint::observe(overlay); }
    reap(child);
    if failed {
        return Err("OpenCode could not be started; install OpenCode 2".into());
    }
    Err("OpenCode is still starting; try again shortly".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_has_no_state_field_and_requires_loopback_and_pid() {
        let reg = parse_registration(include_bytes!(
            "../../../tests/fixtures/opencode2/registration.json"
        ))
        .unwrap();
        assert_eq!(
            (reg.port, reg.pid, reg.version.as_str()),
            (48951, 424242, "2.0.32")
        );
        assert_eq!(reg.password.as_deref(), Some("fixture-password"));
        for bad in [
            br#"{"id":"a","version":"2.0.32","url":"http://10.0.0.5:4096","pid":5}"#.as_slice(),
            br#"{"id":"a","version":"2.0.32","url":"https://127.0.0.1:4096","pid":5}"#,
            br#"{"id":"a","version":"2.0.32","url":"http://127.0.0.1:4096/x","pid":5}"#,
            br#"{"id":"a","version":"2.0.32","url":"http://127.0.0.1:4096","pid":1}"#,
            br#"{"id":"","version":"2.0.32","url":"http://127.0.0.1:4096","pid":5}"#,
        ] {
            assert!(
                parse_registration(bad).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
        assert!(parse_registration(&vec![b' '; 20000]).is_err());
    }

    #[test]
    fn registration_follows_xdg_state_home() {
        let home = Path::new("/home/example");
        assert_eq!(
            registration_path_from(Some("/private/fixture/state"), home),
            PathBuf::from("/private/fixture/state/opencode/service.json")
        );
        assert_eq!(
            registration_path_from(None, home),
            PathBuf::from("/home/example/.local/state/opencode/service.json")
        );
        assert_eq!(
            registration_path_from(Some(""), home),
            PathBuf::from("/home/example/.local/state/opencode/service.json")
        );
    }

    #[test]
    fn versions_compare_numerically_and_reject_other_majors() {
        assert!(version_at_least("2.0.32", MINIMUM));
        assert!(version_at_least("v2.0.40", MINIMUM));
        assert!(version_at_least("2.1.0", MINIMUM));
        assert!(!version_at_least("2.0.31", MINIMUM));
        assert!(!version_at_least("1.18.35", MINIMUM));
        assert!(!version_at_least("3.0.0", MINIMUM));
        assert!(!version_at_least("2.0.33-beta.1", MINIMUM));
        assert!(!version_at_least("garbage", MINIMUM));
    }

    #[test]
    fn missing_operations_names_each_absent_route_or_field() {
        let mut openapi = json!({"paths":{}});
        for (method, path, fields) in REQUIRED {
            openapi["paths"][*path][*method] = if fields.is_empty() {
                json!({})
            } else {
                let properties: serde_json::Map<String, Value> = fields
                    .iter()
                    .map(|f| ((*f).to_owned(), json!({})))
                    .collect();
                json!({"requestBody":{"content":{"application/json":{"schema":{"properties":properties}}}}})
            };
        }
        assert!(
            missing_operations(&openapi).is_empty(),
            "{:?}",
            missing_operations(&openapi)
        );
        openapi["paths"]
            .as_object_mut()
            .unwrap()
            .remove("/api/session/{sessionID}/inbox");
        openapi["paths"]["/api/session/{sessionID}/prompt"]["post"]["requestBody"]["content"]
            ["application/json"]["schema"]["properties"]
            .as_object_mut()
            .unwrap()
            .remove("delivery");
        assert_eq!(
            missing_operations(&openapi),
            vec![
                "POST /api/session/{sessionID}/prompt field delivery".to_owned(),
                "GET /api/session/{sessionID}/inbox".to_owned(),
            ]
        );
    }

    #[test]
    fn non_success_status_is_returned_not_raised() {
        let bytes = b"HTTP/1.1 404 Not Found\r\nContent-Length: 37\r\n\r\n{\"_tag\":\"MessageNotFoundError\",\"x\":1}";
        let response = decode_response(bytes).unwrap();
        assert_eq!(response.status, 404);
        assert_eq!(response.body["_tag"], "MessageNotFoundError");
    }

    #[test]
    fn schema_refs_allof_and_variants_are_resolved() {
        let openapi = json!({
            "paths":{"/x":{"post":{"requestBody":{"content":{"application/json":{"schema":{"$ref":"#/components/schemas/Form.Reply"}}}}}}},
            "components":{"schemas":{
                "Form.Reply":{"type":"object","properties":{"answer":{}}},
                "Merged":{"allOf":[{"$ref":"#/components/schemas/Form.Reply"},{"properties":{"extra":{}}}]},
                "Either":{"oneOf":[{"properties":{"a":{}}},{"properties":{"b":{}}}]},
                "Loop":{"$ref":"#/components/schemas/Loop"}
            }}
        });
        let schema =
            &openapi["paths"]["/x"]["post"]["requestBody"]["content"]["application/json"]["schema"];
        assert!(schema_has_field(&openapi, schema, "answer", 8));
        assert!(!schema_has_field(&openapi, schema, "decision", 8));
        let merged = json!({"$ref":"#/components/schemas/Merged"});
        assert!(schema_has_field(&openapi, &merged, "answer", 8));
        assert!(schema_has_field(&openapi, &merged, "extra", 8));
        let either = json!({"$ref":"#/components/schemas/Either"});
        assert!(schema_has_field(&openapi, &either, "b", 8));
        assert!(!schema_has_field(
            &openapi,
            &json!({"$ref":"#/components/schemas/Loop"}),
            "a",
            8
        ));
        assert!(!schema_has_field(
            &openapi,
            &json!({"$ref":"https://x/y"}),
            "a",
            8
        ));
    }

    #[test]
    fn missing_field_in_a_referenced_schema_is_reported() {
        let mut openapi = json!({"paths":{}, "components":{"schemas":{
            "Body":{"allOf":[{"properties":{"answer":{}}}]},
            "Thin":{"type":"object","properties":{}}
        }}});
        for (method, path, fields) in REQUIRED {
            openapi["paths"][*path][*method] = if fields.is_empty() {
                json!({})
            } else {
                let name = if *path == "/api/session/{sessionID}/form/{formID}/reply" {
                    "Thin"
                } else {
                    "Full"
                };
                json!({"requestBody":{"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{name}")}}}}})
            };
        }
        let properties: serde_json::Map<String, Value> = REQUIRED
            .iter()
            .flat_map(|(_, _, f)| f.iter())
            .map(|f| ((*f).to_owned(), json!({})))
            .collect();
        openapi["components"]["schemas"]["Full"] = json!({"allOf":[{"properties":properties}]});
        assert_eq!(
            missing_operations(&openapi),
            vec!["POST /api/session/{sessionID}/form/{formID}/reply field answer".to_owned()]
        );
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hgs-compat-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn reg() -> Registration {
        Registration {
            id: "a".into(),
            version: "2.0.32".into(),
            port: 4096,
            pid: 5,
            password: None,
        }
    }

    #[test]
    fn requirement_digest_change_misses_the_cache() {
        let dir = temp_dir("digest");
        let body = || Ok(json!({"paths":{}}));
        let first = compatibility_in(&dir, &reg(), "d1", body).unwrap();
        assert!(!first.is_empty());
        // Same digest hits: the fetch must not run.
        assert_eq!(
            compatibility_in(&dir, &reg(), "d1", || Err("no".into())).unwrap(),
            first
        );
        // A different digest misses and fetches again.
        assert!(compatibility_in(&dir, &reg(), "d2", || Err("fetched".into())).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_api_description_is_rejected_and_not_cached() {
        let dir = temp_dir("invalid");
        for bad in [json!("html"), json!([]), json!({"paths":[]}), json!({})] {
            let err = compatibility_in(&dir, &reg(), "d", || Ok(bad)).unwrap_err();
            assert_eq!(err, "OpenCode returned an invalid API description");
        }
        assert!(!dir.exists() || fs::read_dir(&dir).unwrap().next().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_write_failure_keeps_the_verified_verdict() {
        let dir = temp_dir("blocked");
        fs::write(&dir, b"a file where the directory should be").unwrap();
        let missing = compatibility_in(&dir, &reg(), "d", || Ok(json!({"paths":{}}))).unwrap();
        assert!(!missing.is_empty());
        let _ = fs::remove_file(&dir);
    }

    /// A registered port that no service can answer on any platform. Nothing
    /// can listen on port 0, so connecting fails before a byte is sent. A
    /// freed ephemeral port is not safe: a parallel test's server may bind it
    /// and turn the expected refusal into an uncertain delivery.
    fn refusing_port() -> u16 {
        let address = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
        assert!(TcpStream::connect_timeout(&address, Duration::from_secs(2)).is_err());
        0
    }

    #[test]
    fn a_service_that_stopped_answering_is_reported_without_opencode_1_wording() {
        // A port that refuses connections: the registered service went away.
        let gone = Registration {
            port: refusing_port(),
            ..reg()
        };
        let error = request(
            &gone,
            "GET",
            "/api/session/active",
            None,
            Instant::now() + Duration::from_secs(5),
        )
        .err()
        .unwrap();
        assert_eq!(error, "OpenCode is not responding");
        // OpenCode 2 sessions have no Resume to offer.
        assert!(!error.to_lowercase().contains("resume"), "{error}");
    }

    /// A service that accepted the connection and then reset it may have
    /// received the request: that stays an uncertain delivery and is never
    /// reported as a service that refused to connect.
    #[test]
    fn a_reset_after_connecting_stays_uncertain() {
        let server = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = server.local_addr().unwrap().port();
        let worker = std::thread::spawn(move || {
            let (peer, _) = server.accept().unwrap();
            std::thread::sleep(Duration::from_millis(200));
            // Closing with the request unread resets the connection.
            drop(peer);
        });
        let accepted = Registration { port, ..reg() };
        let error = request(
            &accepted,
            "GET",
            "/api/session/active",
            None,
            Instant::now() + Duration::from_secs(5),
        )
        .err()
        .unwrap();
        worker.join().unwrap();
        assert_ne!(error, NOT_RESPONDING);
        assert!(error.contains("delivery uncertain"), "{error}");
    }

    /// The desktop's Start OpenCode banner keys on the "OpenCode is not
    /// running on " prefix, and the documentation quotes both texts.
    #[test]
    fn not_running_and_not_responding_texts_are_pinned() {
        assert_eq!(
            Unavailable::NotRunning.reason("build-01"),
            "OpenCode is not running on build-01"
        );
        assert!(Unavailable::NotRunning
            .reason("fixture")
            .starts_with("OpenCode is not running on "));
        let port = refusing_port();
        // A registration whose process is gone: not running.
        let gone = Registration {
            port,
            pid: 0,
            ..reg()
        };
        let Err(why) = connect_from(RegistrationRead::Present(gone)) else {
            panic!("connected to a service that is gone");
        };
        assert_eq!(
            why.reason("build-01"),
            "OpenCode is not running on build-01"
        );
        // A live registered process that refuses connections: not responding.
        let silent = Registration {
            port,
            pid: std::process::id(),
            ..reg()
        };
        let Err(why) = connect_from(RegistrationRead::Present(silent)) else {
            panic!("connected to a closed port");
        };
        assert!(matches!(&why, Unavailable::Unreachable(_)));
        assert_eq!(why.reason("build-01"), "OpenCode is not responding");
    }

    #[test]
    fn reported_versions_are_clipped_and_control_free() {
        let long = format!("2.0.\u{7}{}", "9".repeat(200));
        let clipped = clipped_version(&long);
        assert_eq!(clipped.chars().count(), 64);
        assert!(!clipped.chars().any(char::is_control));
    }

    #[test]
    fn real_opencode_2_0_32_description_passes_the_gate() {
        let openapi: Value = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/opencode2/openapi-2.0.32.json"
        ))
        .unwrap();
        assert_eq!(missing_operations(&openapi), Vec::<String>::new());
        assert!(verdict(&openapi).unwrap().is_empty());
    }

    #[test]
    fn every_fresh_catalog_route_is_required() {
        let openapi: Value = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/opencode2/openapi-2.0.32.json"
        ))
        .unwrap();
        for path in ["/api/model", "/api/model/default", "/api/provider"] {
            let mut changed = openapi.clone();
            changed["paths"]
                .as_object_mut()
                .unwrap()
                .remove(path)
                .unwrap();
            assert_eq!(missing_operations(&changed), vec![format!("GET {path}")]);
        }
    }

    #[test]
    fn start_is_refused_while_any_registered_process_lives() {
        let present = || {
            RegistrationRead::Present(
                parse_registration(include_bytes!(
                    "../../../tests/fixtures/opencode2/registration.json"
                ))
                .unwrap(),
            )
        };
        assert!(matches!(
            start_decision(&present(), true, false),
            StartDecision::AlreadyRunning
        ));
        assert!(matches!(
            start_decision(&present(), true, true),
            StartDecision::AlreadyRunning
        ));
        assert!(matches!(
            start_decision(&present(), false, false),
            StartDecision::Start
        ));
        assert!(matches!(
            start_decision(&RegistrationRead::Missing, false, false),
            StartDecision::Start
        ));
        assert!(matches!(
            start_decision(&RegistrationRead::Missing, false, true),
            StartDecision::Refuse(_)
        ));
    }

    /// Only a missing registration file counts as "no service"; one that
    /// exists but cannot be read or parsed refuses both connect and start.
    #[test]
    fn only_a_missing_registration_allows_a_start() {
        let dir = temp_dir("registration-read");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("service.json");
        let fixture: &[u8] = include_bytes!("../../../tests/fixtures/opencode2/registration.json");
        let missing = read_registration_at(&path);
        assert!(matches!(missing, RegistrationRead::Missing));
        assert!(matches!(
            start_decision(&missing, false, false),
            StartDecision::Start
        ));
        fs::write(&path, fixture).unwrap();
        let present = read_registration_at(&path);
        assert!(matches!(&present, RegistrationRead::Present(r) if r.id == "svc_fixture"));
        assert!(matches!(
            start_decision(&present, true, false),
            StartDecision::AlreadyRunning
        ));
        assert!(matches!(
            start_decision(&present, false, false),
            StartDecision::Start
        ));
        let mut oversized = fixture.to_vec();
        oversized.extend(std::iter::repeat_n(b' ', 20000));
        for bytes in [&fixture[..fixture.len() / 2], b"not json", &oversized[..]] {
            fs::write(&path, bytes).unwrap();
            let read = read_registration_at(&path);
            assert!(matches!(read, RegistrationRead::Unreadable(_)));
            assert!(matches!(
                start_decision(&read, false, false),
                StartDecision::Refuse(_)
            ));
            assert!(matches!(
                connect_from(read),
                Err(Unavailable::Unreachable(_))
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::write(&path, fixture).unwrap();
            if unsafe { libc::geteuid() } == 0 {
                eprintln!("skipped the mode 000 registration check: root reads any file");
            } else {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
                let read = read_registration_at(&path);
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
                assert!(
                    matches!(&read, RegistrationRead::Unreadable(why) if why == UNREADABLE_REGISTRATION)
                );
                assert!(
                    matches!(start_decision(&read, false, false), StartDecision::Refuse(why) if why == UNREADABLE_REGISTRATION)
                );
                assert!(matches!(
                    connect_from(read),
                    Err(Unavailable::Unreachable(why)) if why == UNREADABLE_REGISTRATION
                ));
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// Works as root and on every platform: a directory in place of the
    /// registration cannot be read (EISDIR, or access denied on Windows).
    #[test]
    fn a_directory_at_the_registration_path_never_starts_the_service() {
        let dir = temp_dir("registration-directory");
        let path = dir.join("service.json");
        fs::create_dir_all(&path).unwrap();
        let read = read_registration_at(&path);
        assert!(
            matches!(&read, RegistrationRead::Unreadable(why) if why == UNREADABLE_REGISTRATION)
        );
        assert!(
            matches!(start_decision(&read, false, false), StartDecision::Refuse(why) if why == UNREADABLE_REGISTRATION)
        );
        assert!(matches!(
            connect_from(read),
            Err(Unavailable::Unreachable(why)) if why == UNREADABLE_REGISTRATION
        ));
        // ensure_started's gate refuses whether the registration became
        // unreadable before its connect or before its second read.
        let reread = || read_registration_at(&path);
        let refused = |gate: Result<Option<Service>>| {
            assert!(matches!(gate, Err(why) if why == UNREADABLE_REGISTRATION));
        };
        refused(may_start(
            connect_from(reread()),
            reread,
            |_| panic!("no pid"),
            || false,
        ));
        refused(may_start(
            Err(Unavailable::NotRunning),
            reread,
            |_| panic!("no pid"),
            || false,
        ));
        refused(may_start(
            Err(Unavailable::Disabled),
            reread,
            |_| panic!("no pid"),
            || false,
        ));
        // Controls: only a missing registration or a dead PID may start.
        fs::remove_dir(&path).unwrap();
        assert!(matches!(
            may_start(
                Err(Unavailable::NotRunning),
                reread,
                |_| panic!("no pid"),
                || false
            ),
            Ok(None)
        ));
        fs::write(
            &path,
            include_bytes!("../../../tests/fixtures/opencode2/registration.json"),
        )
        .unwrap();
        assert!(matches!(
            may_start(Err(Unavailable::NotRunning), reread, |_| false, || false),
            Ok(None)
        ));
        assert!(may_start(Err(Unavailable::NotRunning), reread, |_| true, || false).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn start_environment_drops_session_ssh_and_server_variables() {
        let kept = start_environment(vec![
            ("PATH".into(), "/usr/bin".into()),
            ("HGS_SESSION".into(), "x".into()),
            ("HGS_RUN_ID".into(), "run".into()),
            ("HGS_SESSION_ID".into(), "session".into()),
            ("HGS_CONVERSATION_ID".into(), "conversation".into()),
            ("HGS_STATE_DIR".into(), "/tmp/example-state".into()),
            ("HGS_CONFIG_DIR".into(), "/tmp/example-config".into()),
            ("SSH_AUTH_SOCK".into(), "/s".into()),
            ("SSH_CONNECTION".into(), "1".into()),
            ("OPENCODE_SERVER_PASSWORD".into(), "p".into()),
            ("OPENCODE_CONFIG_CONTENT".into(), "{}".into()),
            ("OPENCODE_PERMISSION".into(), "{}".into()),
            ("OPENCODE_CONFIG".into(), "/home/example/oc.json".into()),
            ("OPENCODE_CONFIG_DIR".into(), "/home/example/oc".into()),
            ("XDG_STATE_HOME".into(), "/home/example/state".into()),
            ("ZERUS_MOBILE_TOKEN".into(), "t".into()),
            ("HOME".into(), "/home/example".into()),
        ]);
        let names: Vec<_> = kept.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "PATH",
                "HGS_STATE_DIR",
                "HGS_CONFIG_DIR",
                "OPENCODE_CONFIG",
                "OPENCODE_CONFIG_DIR",
                "XDG_STATE_HOME",
                "HOME"
            ]
        );
    }
    fn body(bytes: &[u8]) -> Result<Value> {
        decode_response(bytes).map(|response| response.body)
    }
    #[test]
    fn chunked_native_response_is_bounded_and_complete() {
        assert_eq!(
            body(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n7\r\n{\"a\":1}\r\n0\r\n\r\n"
            )
            .unwrap(),
            json!({"a":1})
        );
        assert!(body(b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n{}").is_err());
    }
    #[test]
    fn partial_reads_cannot_extend_the_absolute_native_deadline() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = peer.read(&mut request);
            peer.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\n")
                .unwrap();
            for _ in 0..30 {
                if peer.write_all(b"n").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let started = Instant::now();
        let reg = Registration {
            id: String::new(),
            version: String::new(),
            port,
            pid: 0,
            password: Some("fixture".into()),
        };
        let result = request(
            &reg,
            "GET",
            "/fixture",
            None,
            started + Duration::from_millis(120),
        );
        let elapsed = started.elapsed();
        assert!(result.is_err());
        assert!(elapsed < Duration::from_millis(500), "{elapsed:?}");
        worker.join().unwrap();
    }
    #[test]
    fn malformed_native_http_never_acknowledges_mutations() {
        for raw in [
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nnull\r\n0\r\n".as_slice(),
            b"HTTP/1.1 200 OK\r\nContent-Length: junk\r\n\r\nnull",
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nnull\r\n0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nContent-Length: 4\r\n\r\nnull",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nnullXX0\r\n\r\n",
        ] {assert!(body(raw).is_err(),"{}",String::from_utf8_lossy(raw));}
        assert_eq!(
            body(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap(),
            Value::Null
        );
        assert_eq!(body(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nnull\r\n0\r\nX-Native: yes\r\n\r\n").unwrap(),Value::Null);
    }
}
