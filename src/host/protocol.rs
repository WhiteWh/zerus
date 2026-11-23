//! Protocol 1 between hgs and the session host (spec D5).
//!
//! **Frames.** A connection is a sequence of frames: a `u32` little-endian length,
//! then exactly that many bytes, one kind byte followed by the payload. The length
//! counts the kind byte, so it is at least 1 and at most `MAX_FRAME`. Kind 0 holds
//! JSON (hello, requests, responses), kind 1 terminal bytes and kind 2 stream
//! control JSON. A frame error leaves the connection out of step: close it.
//!
//! **Hello.** The client's first frame is a `Hello` with the protocol majors it
//! speaks; the host answers with a `HelloAnswer`: its identity and the chosen major,
//! or a refusal. The hello shapes never change across majors.
//!
//! **Requests.** Then the client sends `Request`s, a numeric `id` and an `op`, and
//! the host answers each with a `Response` that repeats the id and carries `ok`
//! with the op's payload or `error` with a code and message. Changes within a
//! major are additive: unknown fields are ignored in both directions, an unknown op
//! decodes as `Op::Unsupported` (answered with `unsupported`, so a newer client can
//! degrade), and unknown error codes, roles and control messages decode too.
//!
//! **Stream mode.** After a successful `attach` both sides exchange only kind 1
//! (terminal bytes) and kind 2 (`Control`) frames. After `supervisor_register` the
//! direction turns: the host sends `Request<SupervisorOp>` frames and the
//! supervisor answers each with a `Response`.
use serde::{de::DeserializeOwned, ser::SerializeMap, Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use std::{
    fmt,
    io::{self, Read, Write},
};

/// The largest frame after its length field: the kind byte and the payload.
pub const MAX_FRAME: usize = 1 << 20;

/// What a frame's payload holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A hello, request or response, as JSON.
    Json = 0,
    /// Terminal bytes of an attached client: its input, or the session's output.
    Stream = 1,
    /// A `Control` message of an attached client, as JSON.
    Control = 2,
}

impl Kind {
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Json),
            1 => Some(Self::Stream),
            2 => Some(Self::Control),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Frame {
    pub kind: Kind,
    pub payload: Vec<u8>,
}

impl Frame {
    /// The payload as JSON of type `T`.
    pub fn json<T: DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_slice(&self.payload)
    }
}

/// Why no frame was read or written.
#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    /// The connection ended inside a frame.
    Truncated,
    /// A length of 0: a frame without its kind byte.
    Empty,
    /// A frame longer than `MAX_FRAME`, with its length.
    TooLarge(usize),
    UnknownKind(u8),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Truncated => f.write_str("the session host connection ended inside a frame"),
            Self::Empty => f.write_str("the session host connection sent an empty frame"),
            Self::TooLarge(length) => write!(
                f,
                "a session host frame of {length} bytes exceeds the {MAX_FRAME}-byte limit"
            ),
            Self::UnknownKind(kind) => write!(f, "unknown session host frame kind {kind}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Read until `buffer` is full or the input ends; the number of bytes read.
fn fill<R: Read + ?Sized>(reader: &mut R, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

/// The next frame, or None when the peer closed the connection between frames.
pub fn read_frame<R: Read + ?Sized>(reader: &mut R) -> Result<Option<Frame>, FrameError> {
    let mut length = [0; 4];
    match fill(reader, &mut length)? {
        0 => return Ok(None),
        4 => {}
        _ => return Err(FrameError::Truncated),
    }
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 {
        return Err(FrameError::Empty);
    }
    if length > MAX_FRAME {
        return Err(FrameError::TooLarge(length));
    }
    let mut kind = [0];
    if fill(reader, &mut kind)? == 0 {
        return Err(FrameError::Truncated);
    }
    let kind = Kind::from_byte(kind[0]).ok_or(FrameError::UnknownKind(kind[0]))?;
    let mut payload = vec![0; length - 1];
    if fill(reader, &mut payload)? < payload.len() {
        return Err(FrameError::Truncated);
    }
    Ok(Some(Frame { kind, payload }))
}

/// Write one frame with a single `write_all`, so writers that share a lock never
/// interleave their frames. An oversized payload is refused before anything is
/// written.
pub fn write_frame<W: Write + ?Sized>(
    writer: &mut W,
    kind: Kind,
    payload: &[u8],
) -> Result<(), FrameError> {
    let length = payload.len() + 1;
    if length > MAX_FRAME {
        return Err(FrameError::TooLarge(length));
    }
    let mut frame = Vec::with_capacity(4 + length);
    frame.extend_from_slice(&(length as u32).to_le_bytes());
    frame.push(kind as u8);
    frame.extend_from_slice(payload);
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

/// Write `value` as the JSON payload of one frame.
pub fn write_json<W: Write + ?Sized, T: Serialize + ?Sized>(
    writer: &mut W,
    kind: Kind,
    value: &T,
) -> Result<(), FrameError> {
    let payload = serde_json::to_vec(value).map_err(io::Error::from)?;
    write_frame(writer, kind, &payload)
}

/// The protocol majors one side speaks, both inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Protocols {
    pub min: u32,
    pub max: u32,
}

/// The majors this build speaks. A host accepts every major a released CLI may
/// send, and a CLI speaks every major a running host may still use, so both ends
/// of one version of skew (a rolled-back CLI included) always find one.
pub const PROTOCOLS: Protocols = Protocols { min: 1, max: 1 };

/// Two sides without a common protocol major.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub client: Protocols,
    pub host: Protocols,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "hgs speaks session host protocol {}-{} and the session host {}-{}; \
             install matching versions of hgs",
            self.client.min, self.client.max, self.host.min, self.host.max
        )
    }
}

impl From<Mismatch> for ErrorBody {
    fn from(mismatch: Mismatch) -> Self {
        Self::new(ErrorCode::Unsupported, mismatch.to_string())
    }
}

/// The highest major both sides speak.
pub fn negotiate(client: Protocols, host: Protocols) -> Result<u32, Mismatch> {
    let (low, high) = (client.min.max(host.min), client.max.min(host.max));
    if low <= high {
        Ok(high)
    } else {
        Err(Mismatch { client, host })
    }
}

/// Why a client connects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A command that sends requests and disconnects.
    Cli,
    /// A session's `--run` supervisor, which keeps a supervisor channel (D10).
    Supervisor,
    /// An interactive client that attaches to a session (D13).
    Attach,
    /// A role a newer client named.
    #[serde(other)]
    Other,
}

/// A client's first frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocols: Protocols,
    pub pid: u32,
    /// The client's hgs version.
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
}

impl Hello {
    /// This process's hello.
    pub fn new(role: Role) -> Self {
        Self {
            protocols: PROTOCOLS,
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").into(),
            role: Some(role),
        }
    }
}

/// A Windows integrity level: the RID of a token's mandatory label and its name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Integrity {
    pub name: String,
    pub rid: u32,
}

impl Integrity {
    pub const LOW: u32 = 0x1000;
    pub const MEDIUM: u32 = 0x2000;
    pub const MEDIUM_PLUS: u32 = 0x2100;
    pub const HIGH: u32 = 0x3000;
    pub const SYSTEM: u32 = 0x4000;
    pub const PROTECTED: u32 = 0x5000;

    /// A level named after the well-known level at or below it, as Windows tools
    /// name them.
    pub fn from_rid(rid: u32) -> Self {
        let name = [
            (Self::PROTECTED, "protected"),
            (Self::SYSTEM, "system"),
            (Self::HIGH, "high"),
            (Self::MEDIUM_PLUS, "medium-plus"),
            (Self::MEDIUM, "medium"),
            (Self::LOW, "low"),
        ]
        .into_iter()
        .find(|(level, _)| rid >= *level)
        .map_or("untrusted", |(_, name)| name);
        Self {
            name: name.into(),
            rid,
        }
    }
}

/// The refusal a client below the host's integrity level gets (D5).
pub const ELEVATED_HOST: &str = "the session host on this computer runs elevated";
/// Task #28: what a process in a sandbox below Medium integrity (a DeepSeek
/// member's confined shell) hears instead: the host is not elevated, the
/// caller is below it, and the host keeps such processes out on purpose.
pub const SANDBOXED_CLIENT: &str =
    "this process runs in a sandbox below the session host's integrity level, and the session host keeps sandboxed processes out";

/// The host's identity, sent in answer to an accepted hello.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloReply {
    pub pid: u32,
    /// The host process's creation time as decimal 100 ns ticks, the form
    /// process records use on Windows (D9).
    pub created: String,
    /// The host's executable.
    pub image: String,
    /// The SHA-256 of the executable, lowercase hex.
    pub image_hash: String,
    /// The host's hgs version.
    pub version: String,
    /// The major chosen for this connection.
    pub protocol: u32,
    /// Optional features this host offers, by name.
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub integrity: Integrity,
    /// The Windows session the host runs in.
    pub windows_session: u32,
    /// Whether the host was inside any job when it started, before it created its
    /// own (D4): a launcher fails closed when it was, unless `desktop_job`.
    pub in_job: bool,
    /// Whether that job also holds the shell of the host's desktop, so that the
    /// host ends no earlier than the desktop (D4). Sent only when true, so the
    /// hello of every other host is unchanged; older hosts send none.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub desktop_job: bool,
}

/// The host's answer to a hello: `{"ok": HelloReply}` or `{"error": ErrorBody}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelloAnswer {
    Accepted(HelloReply),
    Refused(ErrorBody),
}

impl Serialize for HelloAnswer {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        match self {
            Self::Accepted(reply) => map.serialize_entry("ok", reply)?,
            Self::Refused(error) => map.serialize_entry("error", error)?,
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for HelloAnswer {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let Value::Object(mut object) = Value::deserialize(deserializer)? else {
            return Err(serde::de::Error::custom("a hello answer is an object"));
        };
        match outcome(&mut object).map_err(serde::de::Error::custom)? {
            Ok(reply) => serde_json::from_value(reply)
                .map(Self::Accepted)
                .map_err(serde::de::Error::custom),
            Err(error) => Ok(Self::Refused(error)),
        }
    }
}

/// What kind of failure a response reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// No such session, pane or client.
    NotFound,
    /// The session name, or another thing to be created, already exists.
    Exists,
    /// The host or the target cannot do it now (`stop_host` while sessions run).
    Busy,
    /// The host does not know the op or option.
    Unsupported,
    /// The host refuses the client or the operation (identity, integrity, input state).
    Refused,
    /// The request is malformed.
    Invalid,
    /// The host failed.
    Internal,
    /// A code a newer host sent.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    /// A message for the user.
    pub message: String,
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for ErrorBody {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// `ok` or `error` of a response object. An `error` wins; a null one is absent.
fn outcome(object: &mut Map<String, Value>) -> Result<Result<Value, ErrorBody>, String> {
    if let Some(error) = object.remove("error").filter(|error| !error.is_null()) {
        return serde_json::from_value(error)
            .map(Err)
            .map_err(|error| format!("invalid error in a session host answer: {error}"));
    }
    object
        .remove("ok")
        .map(Ok)
        .ok_or_else(|| "a session host answer has neither ok nor error".into())
}

/// A session, pane or client as a request names it: a session's exact name, an
/// immutable session ID (`$N`) or a pane ID (`%N`). Every session has one pane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Name(String),
    Session(String),
    Pane(String),
}

/// A terminal size in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

/// Input for a pane: a tmux key name (`Enter`, `Up`, `C-u`, `M-s`) or literal text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    Named(String),
    Literal(String),
}

/// A stop request for a session's agent (D11).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    /// The Unix SIGTERM to the agent: the supervisor raises Ctrl+Break for its
    /// process group. Never forced.
    Pause,
    /// D11's terminate, for `hgs terminate`: pause, wait up to 10 s for the
    /// supervisor's completion record, then end the session as `kill` does.
    Terminate,
}

/// A new session. Retrying with the same `launch_id` never starts a second one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSession {
    pub launch_id: String,
    pub name: String,
    pub cwd: String,
    /// The whole environment block of the session's supervisor as `KEY=VALUE`
    /// strings, in order: the creating client's environment with the session's
    /// overrides (D8). The host never adds its own environment; it sets only
    /// what it alone knows (`HGS_HOST_SESSION`, `HGS_HOST_PANE`) and the
    /// terminal type (`TERM=xterm-256color`, without `COLORTERM`).
    pub env: Vec<String>,
    /// The supervisor's command line, program first.
    pub argv: Vec<String>,
    pub size: Size,
}

/// The agent that should own a pane's input, as `input_state` checks it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputClaim {
    pub target: Target,
    /// The session name the pane must still have.
    pub name: String,
    /// The run marker the pane must show.
    pub run_id: String,
    pub agent: String,
    #[serde(default)]
    pub pid: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attach {
    pub target: Target,
    pub size: Size,
    /// Attach only while the pane shows this run marker (`--existing --run-id`).
    #[serde(default)]
    pub run_id: Option<String>,
}

/// A supervisor offering its channel for the session it supervises (D10).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorRegistration {
    /// `$N`
    pub session: String,
    /// `%N`
    pub pane: String,
    pub pid: u32,
}

/// The session variable that holds a pane's run marker (tmux `@hgs_run`); `list`
/// and `pane_info` report it as `run_marker`.
pub const RUN_MARKER: &str = "@hgs_run";

/// What a client asks of the host. Every op answers with the payload type named
/// in its documentation, `Done` when none is given.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Start a session (`Created`), or report the one this launch ID started.
    CreateSession(CreateSession),
    /// How the session a launch ID started is doing (`LaunchState`), also for
    /// a while after it ended; `not_found` for a launch this host does not
    /// know. A launch reads it to tell a program that could not start from
    /// one that runs.
    LaunchStatus {
        launch_id: String,
    },
    /// Sessions, panes and clients in one round trip (`Listed`).
    List,
    /// A target's session and pane (`Resolved`), or `not_found`.
    ResolveTarget {
        target: Target,
    },
    /// Every pane fact (`PaneDetails`).
    PaneInfo {
        target: Target,
    },
    /// The visible screen, one line per row with trailing spaces trimmed, as
    /// `tmux capture-pane -p` (`-e` when styled) prints it (`Captured`).
    Capture {
        target: Target,
        /// Keep SGR attributes.
        #[serde(default)]
        styled: bool,
    },
    /// Make the pane `rows` tall, keeping its width, while no client is
    /// attached, as tmux `resize-window -y` does for a window without a size
    /// of its own: the next client that attaches sets the size again. It never
    /// shrinks a pane (`Grown`).
    GrowUnviewed {
        target: Target,
        rows: u16,
    },
    /// Keys in order; an empty list only checks that the pane exists.
    SendKeys {
        target: Target,
        keys: Vec<Key>,
    },
    /// Paste text as tmux `paste-buffer -r` does: line feeds included, other
    /// control characters shown as `^X` (tmux 3.7 and later). With `bracketed`,
    /// the text goes inside bracketed-paste markers when the application
    /// enabled them (tmux `paste-buffer -p`).
    Paste {
        target: Target,
        text: String,
        #[serde(default)]
        bracketed: bool,
    },
    /// Set a session variable; a missing or null value removes it. `RUN_MARKER`
    /// is the run marker.
    SetEnvironment {
        target: Target,
        name: String,
        #[serde(default)]
        value: Option<String>,
    },
    /// A session variable (`EnvValue`).
    ShowEnvironment {
        target: Target,
        name: String,
    },
    /// Rename a session; `exists` when another one has the name.
    Rename {
        target: Target,
        name: String,
    },
    /// End a session without the pause request, as tmux `kill-session` hangs up
    /// a pane: close its pseudoconsole (`CTRL_CLOSE_EVENT`, which leaves the
    /// supervisor about 4 s to record the exit), then end its job. A dead pane is
    /// removed. Like `signal`, it answers once the stop has begun; the session
    /// leaves `list` when it has ended.
    Kill {
        target: Target,
    },
    Signal {
        target: Target,
        signal: Signal,
    },
    /// Whether the claimed agent owns the pane's input now (`InputReady`), or
    /// `refused` with the reason for the user.
    InputState(InputClaim),
    /// Attach this connection (`Attached`), which then carries stream frames.
    Attach(Attach),
    /// Attached clients, of one session when a target is given (`ClientList`).
    ListClients {
        #[serde(default)]
        target: Option<Target>,
    },
    /// Show a short message on an attached client.
    Notify {
        client: String,
        text: String,
    },
    /// `HostStatus`.
    HostStatus,
    /// Stop an idle host; `busy` while sessions run.
    StopHost,
    /// Turn this connection into the supervisor channel of a session.
    SupervisorRegister(SupervisorRegistration),
    /// Supervise the recovery worker, the stable CLI's `hgs.exe recovery
    /// worker`, until the host exits (D14): started now unless it runs,
    /// restarted after it exits. `unsupported` where the host supervises none.
    RecoveryWorker,
    /// Task #33: the session whose job holds this connection's own client
    /// process, as the kernel names the client (`CallerSession`); `not_found`
    /// outside every session. It proves the caller where its process chain
    /// broke: a Git Bash wrapper (`bash -c`, `env`, `timeout`, ...) ends the
    /// Windows process between the session and hgs, but never leaves the job.
    CallerSession,
    /// An op this host does not know; answered with `unsupported`.
    #[serde(other)]
    Unsupported,
}

/// What the host asks a supervisor over its channel (D10, D11).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SupervisorOp {
    /// The console evidence for an input check (`ConsoleEvidence`).
    InputState,
    /// The pause request: raise Ctrl+Break for the supervisor's process group.
    Stop,
    #[serde(other)]
    Unsupported,
}

/// A request: `{"id": N, "op": "...", ...}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Request<O = Op> {
    pub id: u64,
    #[serde(flatten)]
    pub op: O,
}

/// A request that could not be decoded. With its `id` known the host answers it
/// as `invalid`; without one the connection is out of step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BadRequest {
    pub id: Option<u64>,
    pub message: String,
}

impl BadRequest {
    /// The answer to send, when the request named an id.
    pub fn response(&self) -> Option<Response> {
        self.id
            .map(|id| Response::error(id, ErrorCode::Invalid, self.message.clone()))
    }
}

impl<O: DeserializeOwned> Request<O> {
    pub fn decode(payload: &[u8]) -> Result<Self, BadRequest> {
        let value = serde_json::from_slice(payload).map_err(|error| BadRequest {
            id: None,
            message: format!("invalid session host request: {error}"),
        })?;
        Self::from_value(value)
    }

    fn from_value(value: Value) -> Result<Self, BadRequest> {
        let Some(id) = value.get("id").and_then(Value::as_u64) else {
            return Err(BadRequest {
                id: None,
                message: "a session host request needs a numeric id".into(),
            });
        };
        let op = O::deserialize(value).map_err(|error| BadRequest {
            id: Some(id),
            message: format!("invalid session host request: {error}"),
        })?;
        Ok(Self { id, op })
    }
}

impl<'de, O: DeserializeOwned> Deserialize<'de> for Request<O> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_value(Value::deserialize(deserializer)?)
            .map_err(|bad| serde::de::Error::custom(bad.message))
    }
}

/// The answer to request `id`: `{"id": N, "ok": payload}` or
/// `{"id": N, "error": {"code": ..., "message": ...}}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub id: u64,
    pub result: Result<Value, ErrorBody>,
}

impl Response {
    pub fn ok(id: u64, payload: &impl Serialize) -> Self {
        match serde_json::to_value(payload) {
            Ok(payload) => Self {
                id,
                result: Ok(payload),
            },
            Err(error) => Self::error(id, ErrorCode::Internal, error.to_string()),
        }
    }

    pub fn error(id: u64, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            id,
            result: Err(ErrorBody::new(code, message)),
        }
    }

    /// The payload as `T`; a payload of another shape is an `invalid` error.
    pub fn payload<T: DeserializeOwned>(self) -> Result<T, ErrorBody> {
        serde_json::from_value(self.result?).map_err(|error| {
            ErrorBody::new(
                ErrorCode::Invalid,
                format!("invalid session host answer: {error}"),
            )
        })
    }
}

impl Serialize for Response {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("id", &self.id)?;
        match &self.result {
            Ok(payload) => map.serialize_entry("ok", payload)?,
            Err(error) => map.serialize_entry("error", error)?,
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Response {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let Value::Object(mut object) = Value::deserialize(deserializer)? else {
            return Err(serde::de::Error::custom("a response is an object"));
        };
        let id = object
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| serde::de::Error::custom("a response needs a numeric id"))?;
        let result = outcome(&mut object).map_err(serde::de::Error::custom)?;
        Ok(Self { id, result })
    }
}

/// The answer to an op without a payload: `{}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Done {}

/// `create_session`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Created {
    /// `$N`
    pub session: String,
    /// `%N`
    pub pane: String,
    pub name: String,
    /// The pane's root process: the `--run` supervisor.
    pub pid: u32,
    /// Whether an earlier request with this launch ID had started it.
    pub existing: bool,
}

/// `launch_status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchState {
    /// `$N`
    pub session: String,
    /// `%N`
    pub pane: String,
    pub name: String,
    /// The root process has exited (`dead` of its pane).
    pub dead: bool,
    /// The session has ended: its console has closed and its output drained.
    pub ended: bool,
    /// Once the session has ended, its root process's exit code when known.
    #[serde(default)]
    pub code: Option<i32>,
    /// Once the session has ended, the last lines its pane showed, wrapped
    /// rows joined: what a program that could not start said.
    #[serde(default)]
    pub output: String,
}

/// One session of `list`: what `tmux list-sessions` reports today.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEntry {
    /// `$N`
    pub id: String,
    pub name: String,
    /// Attached clients.
    pub attached: u64,
    /// When the session started, in Unix seconds.
    pub created: u64,
    pub launch_id: String,
}

/// One pane of `list`: what `tmux list-panes -a` reports today.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneEntry {
    /// The session's name.
    pub session: String,
    /// `$N`
    pub session_id: String,
    /// `%N`
    pub pane: String,
    /// The supervisor's process ID.
    pub pid: u32,
    /// Whether the pane's process has exited.
    pub dead: bool,
    /// The run ID its supervisor set ("" when untracked).
    pub run_marker: String,
    /// The pane's current directory when known, else "" (passive metadata).
    /// The host reports the directory the session started in: it does not
    /// read another process's current directory.
    pub current_path: String,
    /// Task #14 (P2.4): raised by every output of the pane and when its
    /// process exits. None from a host that predates it; an older client
    /// ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<u64>,
}

/// An attached client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientEntry {
    /// An opaque ID; `ls --json` lists it where tmux lists a tty (D13).
    pub id: String,
    /// The name of the session it shows.
    pub session: String,
    pub pid: u32,
}

/// `list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listed {
    pub sessions: Vec<SessionEntry>,
    pub panes: Vec<PaneEntry>,
    pub clients: Vec<ClientEntry>,
    /// Task #14 (P2.4): raised when a session starts, ends, is renamed or a
    /// client attaches or leaves.
    /// None from a host that predates it; an older client ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structure: Option<u64>,
}

/// `resolve_target`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolved {
    /// `$N`
    pub session: String,
    pub name: String,
    /// `%N`
    pub pane: String,
}

/// `caller_session`: the session that holds the asking process, with the run
/// marker its launch recorded.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallerSession {
    /// `$N`
    pub session: String,
    pub name: String,
    /// `%N`
    pub pane: String,
    #[serde(default)]
    pub run_marker: String,
}

/// `pane_info`: every fact the backend's `PaneField` names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneDetails {
    /// `$N`
    pub session_id: String,
    pub session_name: String,
    /// `%N`
    pub pane: String,
    /// The host's process ID.
    pub server_pid: u32,
    /// The supervisor's process ID.
    pub pane_pid: u32,
    pub width: u16,
    pub height: u16,
    /// In cells.
    pub cursor_x: u16,
    pub cursor_y: u16,
    /// Always false: the host has no copy or view mode.
    pub in_mode: bool,
    /// The program in the foreground of the pane, as a file stem.
    pub current_command: String,
    pub dead: bool,
    pub run_marker: String,
    /// Whether the application enabled bracketed paste.
    pub bracketed_paste: bool,
    /// An opaque name for the pane's console.
    pub tty: String,
}

/// `capture`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Captured {
    pub text: String,
}

/// `grow_unviewed`: whether the pane is now taller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grown {
    pub grown: bool,
}

/// `show_environment`: null when the variable is not set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvValue {
    pub value: Option<String>,
}

/// `input_state`: the cursor, in cells, of a pane whose agent owns its input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputReady {
    pub cursor_x: u16,
    pub cursor_y: u16,
}

/// `attach`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attached {
    /// This connection's client ID.
    pub client: String,
    /// `$N`
    pub session: String,
    pub name: String,
    /// `%N`
    pub pane: String,
}

/// `list_clients`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientList {
    pub clients: Vec<ClientEntry>,
}

/// `host_status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostStatus {
    pub sessions: u32,
    pub clients: u32,
    /// `bundled` or `inbox` (D6).
    pub conpty: String,
}

/// A stream control message of an attached connection (kind 2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    /// Client to host: the client terminal's new size. The session follows the
    /// most recent client (D13).
    Resize(Size),
    /// Either way: the client leaves; the session runs on.
    Detach,
    /// Host to client: the session ended, with its supervisor's exit code when known.
    Exit {
        #[serde(default)]
        code: Option<i32>,
    },
    /// Host to client: a short message to show.
    Notice { text: String },
    /// A message a newer peer sent; ignore it.
    #[serde(other)]
    Unknown,
}

/// A process attached to a session's console.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleProcess {
    pub pid: u32,
    /// The image's file name.
    pub name: String,
}

/// A supervisor's answer to `SupervisorOp::InputState`: the evidence the host
/// judges together with its screen model (D10).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleEvidence {
    /// `ENABLE_LINE_INPUT` of the console input mode (the counterpart of ICANON).
    pub line_input: bool,
    /// `ENABLE_ECHO_INPUT` (the counterpart of ECHO).
    pub echo_input: bool,
    /// `ENABLE_PROCESSED_INPUT` (the counterpart of ISIG): reported, not
    /// judged, as D10 asks only for line and echo input to be off.
    #[serde(default)]
    pub processed_input: bool,
    /// `ENABLE_VIRTUAL_TERMINAL_INPUT`: the program reads VT input rather
    /// than key records, which decides how Ctrl+letter key sends are written
    /// (D6). None from a supervisor that does not report it.
    #[serde(default)]
    pub virtual_terminal_input: Option<bool>,
    /// `GetConsoleProcessList`.
    pub processes: Vec<ConsoleProcess>,
    /// The tracked agent, once started.
    #[serde(default)]
    pub agent_pid: Option<u32>,
    pub agent_alive: bool,
    /// Whether the agent is inside the session job.
    pub agent_in_job: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fmt::Debug;

    /// `value` serializes to the JSON of `text` (names, nesting, nulls and number
    /// types; key order carries no meaning) and `text` decodes to `value`. A change
    /// here breaks peers of another version.
    fn golden<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T, text: &str) {
        assert_eq!(
            serde_json::to_value(value).unwrap(),
            serde_json::from_str::<Value>(text).unwrap(),
            "{text}"
        );
        assert_eq!(&serde_json::from_str::<T>(text).unwrap(), value);
    }

    fn frame_bytes(frames: &[(Kind, &[u8])]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (kind, payload) in frames {
            write_frame(&mut bytes, *kind, payload).unwrap();
        }
        bytes
    }

    /// A reader that returns at most one byte per call and is interrupted between
    /// bytes, as a pipe may be.
    struct Trickle<'a> {
        bytes: &'a [u8],
        interrupt: bool,
    }
    impl Read for Trickle<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.interrupt = !self.interrupt;
            if self.interrupt {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let Some((first, rest)) = self.bytes.split_first() else {
                return Ok(0);
            };
            buffer[0] = *first;
            self.bytes = rest;
            Ok(1)
        }
    }

    #[test]
    fn frames_carry_a_length_that_counts_the_kind_byte() {
        assert_eq!(
            frame_bytes(&[(Kind::Json, b"{}")]),
            [3, 0, 0, 0, 0, b'{', b'}']
        );
        assert_eq!(frame_bytes(&[(Kind::Stream, b"")]), [1, 0, 0, 0, 1]);
        assert_eq!(frame_bytes(&[(Kind::Control, b"x")]), [2, 0, 0, 0, 2, b'x']);

        let large = vec![7; MAX_FRAME - 1];
        let bytes = frame_bytes(&[
            (Kind::Json, b"{\"id\":1}"),
            (Kind::Stream, b"\x1b[0m"),
            (Kind::Control, b""),
            (Kind::Stream, &large),
        ]);
        assert_eq!(bytes[bytes.len() - MAX_FRAME - 4..][..4], [0, 0, 16, 0]);
        for reader in [
            &mut bytes.as_slice() as &mut dyn Read,
            &mut Trickle {
                bytes: &bytes,
                interrupt: false,
            },
        ] {
            let mut next = || read_frame(reader).unwrap().unwrap();
            assert_eq!(next().json::<Value>().unwrap(), json!({"id": 1}));
            assert_eq!(
                next(),
                Frame {
                    kind: Kind::Stream,
                    payload: b"\x1b[0m".to_vec()
                }
            );
            assert_eq!(next().kind, Kind::Control);
            assert_eq!(next().payload, large);
            assert!(read_frame(reader).unwrap().is_none());
        }
    }

    #[test]
    fn bad_frames_fail_with_their_reason() {
        let mut written = Vec::new();
        let error = write_frame(&mut written, Kind::Stream, &vec![0; MAX_FRAME]).unwrap_err();
        assert!(matches!(error, FrameError::TooLarge(length) if length == MAX_FRAME + 1));
        assert!(written.is_empty(), "nothing of a refused frame is written");
        assert!(error.to_string().contains("exceeds"));

        let read = |bytes: &[u8]| read_frame(&mut &bytes[..]);
        assert!(read(&[]).unwrap().is_none(), "end of input between frames");
        for truncated in [&[3][..], &[3, 0, 0], &[3, 0, 0, 0], &[3, 0, 0, 0, 0, b'{']] {
            assert!(
                matches!(read(truncated), Err(FrameError::Truncated)),
                "{truncated:?}"
            );
        }
        assert!(matches!(read(&[0, 0, 0, 0]), Err(FrameError::Empty)));
        assert!(matches!(
            read(&[1, 0, 16, 0, 0]),
            Err(FrameError::TooLarge(length)) if length == MAX_FRAME + 1
        ));
        assert!(matches!(
            read(&[255, 255, 255, 255]),
            Err(FrameError::TooLarge(_))
        ));
        assert!(matches!(
            read(&[1, 0, 0, 0, 3]),
            Err(FrameError::UnknownKind(3))
        ));
        assert!(matches!(
            read_frame(&mut Failing),
            Err(FrameError::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe
        ));

        struct Failing;
        impl Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
    }

    #[test]
    fn negotiation_picks_the_highest_common_major() {
        let range = |min, max| Protocols { min, max };
        assert_eq!(negotiate(PROTOCOLS, PROTOCOLS), Ok(1));
        assert_eq!(negotiate(range(1, 3), range(2, 5)), Ok(3));
        assert_eq!(negotiate(range(2, 5), range(1, 3)), Ok(3));
        assert_eq!(negotiate(range(1, 1), range(1, 2)), Ok(1));
        let mismatch = negotiate(range(2, 3), range(1, 1)).unwrap_err();
        assert_eq!(
            mismatch,
            Mismatch {
                client: range(2, 3),
                host: range(1, 1)
            }
        );
        assert!(negotiate(range(3, 2), range(1, 5)).is_err(), "empty range");
        let body = ErrorBody::from(mismatch);
        assert_eq!(body.code, ErrorCode::Unsupported);
        assert_eq!(
            body.message,
            "hgs speaks session host protocol 2-3 and the session host 1-1; \
             install matching versions of hgs"
        );
    }

    #[test]
    fn hello_shapes_are_stable() {
        let hello = Hello::new(Role::Cli);
        assert_eq!(hello.protocols, PROTOCOLS);
        assert_eq!(hello.pid, std::process::id());
        assert_eq!(hello.version, env!("CARGO_PKG_VERSION"));
        golden(
            &Hello {
                protocols: PROTOCOLS,
                pid: 4242,
                version: "1.52.0".into(),
                role: Some(Role::Supervisor),
            },
            r#"{"protocols":{"min":1,"max":1},"pid":4242,"version":"1.52.0","role":"supervisor"}"#,
        );
        // An older client sends no role, a newer one an unknown role and fields.
        let hello: Hello = serde_json::from_str(
            r#"{"protocols":{"min":1,"max":2},"pid":1,"version":"9","role":"robot","new":true}"#,
        )
        .unwrap();
        assert_eq!(hello.role, Some(Role::Other));
        let hello: Hello =
            serde_json::from_str(r#"{"protocols":{"min":1,"max":1},"pid":1,"version":"9"}"#)
                .unwrap();
        assert_eq!(hello.role, None);
        assert!(!serde_json::to_string(&hello).unwrap().contains("role"));

        let reply = HelloReply {
            pid: 900,
            created: "133731234567890123".into(),
            image: r"C:\Users\a\.local\share\hgs\host\0123456789abcdef\hgs.exe".into(),
            image_hash: "ab".repeat(32),
            version: "1.52.0".into(),
            protocol: 1,
            capabilities: vec!["conpty-bundled".into()],
            integrity: Integrity::from_rid(Integrity::MEDIUM),
            windows_session: 1,
            in_job: false,
            desktop_job: false,
        };
        golden(
            &HelloAnswer::Accepted(reply.clone()),
            &format!(
                r#"{{"ok":{{"pid":900,"created":"133731234567890123","image":"C:\\Users\\a\\.local\\share\\hgs\\host\\0123456789abcdef\\hgs.exe","image_hash":"{}","version":"1.52.0","protocol":1,"capabilities":["conpty-bundled"],"integrity":{{"name":"medium","rid":8192}},"windows_session":1,"in_job":false}}}}"#,
                "ab".repeat(32)
            ),
        );
        golden(
            &HelloAnswer::Refused(ErrorBody::new(ErrorCode::Refused, ELEVATED_HOST)),
            r#"{"error":{"code":"refused","message":"the session host on this computer runs elevated"}}"#,
        );
        // A newer host adds fields; an answer without ok or error is malformed.
        let mut newer = serde_json::to_value(HelloAnswer::Accepted(reply.clone())).unwrap();
        newer["ok"]["uptime"] = json!(5);
        newer["trace"] = json!("x");
        assert_eq!(
            serde_json::from_value::<HelloAnswer>(newer).unwrap(),
            HelloAnswer::Accepted(reply)
        );
        assert!(serde_json::from_str::<HelloAnswer>(r#"{"trace":"x"}"#).is_err());
        assert!(serde_json::from_str::<HelloAnswer>("[]").is_err());
    }

    #[test]
    fn integrity_levels_are_named_like_windows_tools() {
        let name = |rid| Integrity::from_rid(rid).name;
        assert_eq!(name(0), "untrusted");
        assert_eq!(name(Integrity::LOW), "low");
        assert_eq!(name(Integrity::MEDIUM), "medium");
        assert_eq!(name(Integrity::MEDIUM_PLUS), "medium-plus");
        assert_eq!(name(Integrity::HIGH), "high");
        assert_eq!(name(Integrity::HIGH + 0x100), "high");
        assert_eq!(name(Integrity::SYSTEM), "system");
        assert_eq!(name(Integrity::PROTECTED), "protected");
        assert_eq!(Integrity::from_rid(0x3000).rid, 0x3000);
    }

    #[test]
    fn requests_have_stable_json() {
        let target = || Target::Pane("%3".into());
        let request = |id, op| Request { id, op };
        let cases: Vec<(Request, &str)> = vec![
            (
                request(
                    1,
                    Op::CreateSession(CreateSession {
                        launch_id: "launch-1".into(),
                        name: "claude/p/a".into(),
                        cwd: r"C:\src\p".into(),
                        env: vec!["=C:=C:\\src".into(), "Path=C:\\Windows".into()],
                        argv: vec![r"C:\Users\a\.local\bin\hgs.exe".into(), "--run".into()],
                        size: Size {
                            cols: 120,
                            rows: 40,
                        },
                    }),
                ),
                r#"{"id":1,"op":"create_session","launch_id":"launch-1","name":"claude/p/a","cwd":"C:\\src\\p","env":["=C:=C:\\src","Path=C:\\Windows"],"argv":["C:\\Users\\a\\.local\\bin\\hgs.exe","--run"],"size":{"cols":120,"rows":40}}"#,
            ),
            (request(2, Op::List), r#"{"id":2,"op":"list"}"#),
            (
                request(
                    3,
                    Op::ResolveTarget {
                        target: Target::Name("claude/p/a".into()),
                    },
                ),
                r#"{"id":3,"op":"resolve_target","target":{"name":"claude/p/a"}}"#,
            ),
            (
                request(
                    4,
                    Op::PaneInfo {
                        target: Target::Session("$2".into()),
                    },
                ),
                r#"{"id":4,"op":"pane_info","target":{"session":"$2"}}"#,
            ),
            (
                request(
                    5,
                    Op::Capture {
                        target: target(),
                        styled: true,
                    },
                ),
                r#"{"id":5,"op":"capture","target":{"pane":"%3"},"styled":true}"#,
            ),
            (
                request(
                    6,
                    Op::SendKeys {
                        target: target(),
                        keys: vec![Key::Literal("/model".into()), Key::Named("Enter".into())],
                    },
                ),
                r#"{"id":6,"op":"send_keys","target":{"pane":"%3"},"keys":[{"literal":"/model"},{"named":"Enter"}]}"#,
            ),
            (
                request(
                    7,
                    Op::Paste {
                        target: target(),
                        text: "a\nb".into(),
                        bracketed: true,
                    },
                ),
                r#"{"id":7,"op":"paste","target":{"pane":"%3"},"text":"a\nb","bracketed":true}"#,
            ),
            (
                request(
                    8,
                    Op::SetEnvironment {
                        target: Target::Name("claude/p/a".into()),
                        name: RUN_MARKER.into(),
                        value: Some("run-1".into()),
                    },
                ),
                r#"{"id":8,"op":"set_environment","target":{"name":"claude/p/a"},"name":"@hgs_run","value":"run-1"}"#,
            ),
            (
                request(
                    9,
                    Op::ShowEnvironment {
                        target: Target::Name("claude/p/a".into()),
                        name: "HGS_CLIENT".into(),
                    },
                ),
                r#"{"id":9,"op":"show_environment","target":{"name":"claude/p/a"},"name":"HGS_CLIENT"}"#,
            ),
            (
                request(
                    10,
                    Op::Rename {
                        target: Target::Session("$2".into()),
                        name: "claude/p/b".into(),
                    },
                ),
                r#"{"id":10,"op":"rename","target":{"session":"$2"},"name":"claude/p/b"}"#,
            ),
            (
                request(11, Op::Kill { target: target() }),
                r#"{"id":11,"op":"kill","target":{"pane":"%3"}}"#,
            ),
            (
                request(
                    12,
                    Op::Signal {
                        target: target(),
                        signal: Signal::Pause,
                    },
                ),
                r#"{"id":12,"op":"signal","target":{"pane":"%3"},"signal":"pause"}"#,
            ),
            (
                request(
                    13,
                    Op::InputState(InputClaim {
                        target: target(),
                        name: "claude/p/a".into(),
                        run_id: "run-1".into(),
                        agent: "claude".into(),
                        pid: Some(77),
                    }),
                ),
                r#"{"id":13,"op":"input_state","target":{"pane":"%3"},"name":"claude/p/a","run_id":"run-1","agent":"claude","pid":77}"#,
            ),
            (
                request(
                    14,
                    Op::Attach(Attach {
                        target: Target::Name("claude/p/a".into()),
                        size: Size { cols: 80, rows: 24 },
                        run_id: None,
                    }),
                ),
                r#"{"id":14,"op":"attach","target":{"name":"claude/p/a"},"size":{"cols":80,"rows":24},"run_id":null}"#,
            ),
            (
                request(15, Op::ListClients { target: None }),
                r#"{"id":15,"op":"list_clients","target":null}"#,
            ),
            (
                request(
                    16,
                    Op::Notify {
                        client: "c1".into(),
                        text: "Pasted".into(),
                    },
                ),
                r#"{"id":16,"op":"notify","client":"c1","text":"Pasted"}"#,
            ),
            (
                request(17, Op::HostStatus),
                r#"{"id":17,"op":"host_status"}"#,
            ),
            (request(18, Op::StopHost), r#"{"id":18,"op":"stop_host"}"#),
            (
                request(
                    19,
                    Op::SupervisorRegister(SupervisorRegistration {
                        session: "$2".into(),
                        pane: "%3".into(),
                        pid: 77,
                    }),
                ),
                r#"{"id":19,"op":"supervisor_register","session":"$2","pane":"%3","pid":77}"#,
            ),
            (
                request(
                    20,
                    Op::Signal {
                        target: target(),
                        signal: Signal::Terminate,
                    },
                ),
                r#"{"id":20,"op":"signal","target":{"pane":"%3"},"signal":"terminate"}"#,
            ),
            (
                request(21, Op::RecoveryWorker),
                r#"{"id":21,"op":"recovery_worker"}"#,
            ),
            (
                request(
                    22,
                    Op::LaunchStatus {
                        launch_id: "launch-1".into(),
                    },
                ),
                r#"{"id":22,"op":"launch_status","launch_id":"launch-1"}"#,
            ),
        ];
        for (request, text) in &cases {
            golden(request, text);
            assert_eq!(&Request::decode(text.as_bytes()).unwrap(), request);
        }
    }

    #[test]
    fn requests_degrade_instead_of_failing() {
        // Optional fields may be left out.
        let request: Request =
            Request::decode(br#"{"id":5,"op":"capture","target":{"pane":"%3"}}"#).unwrap();
        assert_eq!(
            request.op,
            Op::Capture {
                target: Target::Pane("%3".into()),
                styled: false
            }
        );
        // Unknown fields of a known op are ignored.
        let request: Request =
            Request::decode(br#"{"id":2,"op":"list","since":5,"verbose":true}"#).unwrap();
        assert_eq!(request.op, Op::List);
        // An unknown op is a request the host answers with `unsupported`.
        let request: Request =
            Request::decode(br#"{"id":9,"op":"resize_pane","target":{"pane":"%3"}}"#).unwrap();
        assert_eq!((request.id, request.op), (9, Op::Unsupported));
        let request: Request<SupervisorOp> =
            Request::decode(br#"{"id":4,"op":"screenshot"}"#).unwrap();
        assert_eq!(request.op, SupervisorOp::Unsupported);

        // A known op with a bad payload keeps its id for the `invalid` answer.
        let bad = Request::<Op>::decode(br#"{"id":7,"op":"kill"}"#).unwrap_err();
        assert_eq!(bad.id, Some(7));
        let response = bad.response().unwrap();
        assert_eq!(response.id, 7);
        assert_eq!(response.result.unwrap_err().code, ErrorCode::Invalid);
        for broken in [
            &br#"{"op":"list"}"#[..],
            br#"{"id":"7","op":"list"}"#,
            br#"{"id":-1,"op":"list"}"#,
            b"[1]",
            b"{",
        ] {
            let bad = Request::<Op>::decode(broken).unwrap_err();
            assert_eq!(bad.id, None, "{}", String::from_utf8_lossy(broken));
            assert!(bad.response().is_none());
        }
        assert!(Request::<Op>::decode(br#"{"id":3}"#).unwrap_err().id == Some(3));
        // The same rules hold when a request is read as JSON of a frame.
        let frame = Frame {
            kind: Kind::Json,
            payload: br#"{"id":2,"op":"list","new":1}"#.to_vec(),
        };
        assert_eq!(
            frame.json::<Request>().unwrap(),
            Request {
                id: 2,
                op: Op::List
            }
        );
        let frame = Frame {
            kind: Kind::Json,
            payload: br#"{"op":"list"}"#.to_vec(),
        };
        assert!(frame.json::<Request>().is_err());
    }

    #[test]
    fn supervisor_channel_messages_have_stable_json() {
        golden(
            &Request {
                id: 1,
                op: SupervisorOp::InputState,
            },
            r#"{"id":1,"op":"input_state"}"#,
        );
        golden(
            &Request {
                id: 2,
                op: SupervisorOp::Stop,
            },
            r#"{"id":2,"op":"stop"}"#,
        );
        golden(
            &Response::ok(
                1,
                &ConsoleEvidence {
                    line_input: false,
                    echo_input: false,
                    processed_input: true,
                    virtual_terminal_input: Some(false),
                    processes: vec![
                        ConsoleProcess {
                            pid: 77,
                            name: "hgs.exe".into(),
                        },
                        ConsoleProcess {
                            pid: 78,
                            name: "node.exe".into(),
                        },
                    ],
                    agent_pid: Some(78),
                    agent_alive: true,
                    agent_in_job: true,
                },
            ),
            r#"{"id":1,"ok":{"line_input":false,"echo_input":false,"processed_input":true,"virtual_terminal_input":false,"processes":[{"pid":77,"name":"hgs.exe"},{"pid":78,"name":"node.exe"}],"agent_pid":78,"agent_alive":true,"agent_in_job":true}}"#,
        );
        // A supervisor that reports no processed or VT input flag (an older one).
        let evidence: ConsoleEvidence = serde_json::from_str(
            r#"{"line_input":true,"echo_input":true,"processes":[],"agent_alive":false,"agent_in_job":false}"#,
        )
        .unwrap();
        assert!(!evidence.processed_input && evidence.agent_pid.is_none());
        assert_eq!(evidence.virtual_terminal_input, None);
    }

    #[test]
    fn responses_have_stable_json() {
        let session = || SessionEntry {
            id: "$2".into(),
            name: "claude/p/a".into(),
            attached: 1,
            created: 1_760_000_000,
            launch_id: "launch-1".into(),
        };
        let pane = || PaneEntry {
            session: "claude/p/a".into(),
            session_id: "$2".into(),
            pane: "%3".into(),
            pid: 77,
            dead: false,
            run_marker: "run-1".into(),
            current_path: r"C:\src\p".into(),
            output: None,
        };
        let client = || ClientEntry {
            id: "c1".into(),
            session: "claude/p/a".into(),
            pid: 90,
        };
        let cases: Vec<(Response, &str)> = vec![
            (
                Response::ok(
                    1,
                    &Created {
                        session: "$2".into(),
                        pane: "%3".into(),
                        name: "claude/p/a".into(),
                        pid: 77,
                        existing: false,
                    },
                ),
                r#"{"id":1,"ok":{"session":"$2","pane":"%3","name":"claude/p/a","pid":77,"existing":false}}"#,
            ),
            (
                Response::ok(
                    2,
                    &Listed {
                        sessions: vec![session()],
                        panes: vec![pane()],
                        clients: vec![client()],
                        structure: None,
                    },
                ),
                r#"{"id":2,"ok":{"sessions":[{"id":"$2","name":"claude/p/a","attached":1,"created":1760000000,"launch_id":"launch-1"}],"panes":[{"session":"claude/p/a","session_id":"$2","pane":"%3","pid":77,"dead":false,"run_marker":"run-1","current_path":"C:\\src\\p"}],"clients":[{"id":"c1","session":"claude/p/a","pid":90}]}}"#,
            ),
            (
                Response::ok(
                    3,
                    &Resolved {
                        session: "$2".into(),
                        name: "claude/p/a".into(),
                        pane: "%3".into(),
                    },
                ),
                r#"{"id":3,"ok":{"session":"$2","name":"claude/p/a","pane":"%3"}}"#,
            ),
            (
                Response::ok(
                    4,
                    &PaneDetails {
                        session_id: "$2".into(),
                        session_name: "claude/p/a".into(),
                        pane: "%3".into(),
                        server_pid: 900,
                        pane_pid: 77,
                        width: 120,
                        height: 40,
                        cursor_x: 2,
                        cursor_y: 38,
                        in_mode: false,
                        current_command: "node".into(),
                        dead: false,
                        run_marker: "run-1".into(),
                        bracketed_paste: true,
                        tty: "console-3".into(),
                    },
                ),
                r#"{"id":4,"ok":{"session_id":"$2","session_name":"claude/p/a","pane":"%3","server_pid":900,"pane_pid":77,"width":120,"height":40,"cursor_x":2,"cursor_y":38,"in_mode":false,"current_command":"node","dead":false,"run_marker":"run-1","bracketed_paste":true,"tty":"console-3"}}"#,
            ),
            (
                Response::ok(
                    5,
                    &Captured {
                        text: "\u{1b}[1m>\u{1b}[0m\n".into(),
                    },
                ),
                r#"{"id":5,"ok":{"text":"\u001b[1m>\u001b[0m\n"}}"#,
            ),
            (
                Response::ok(9, &EnvValue { value: None }),
                r#"{"id":9,"ok":{"value":null}}"#,
            ),
            (
                Response::ok(
                    13,
                    &InputReady {
                        cursor_x: 2,
                        cursor_y: 38,
                    },
                ),
                r#"{"id":13,"ok":{"cursor_x":2,"cursor_y":38}}"#,
            ),
            (
                Response::ok(
                    14,
                    &Attached {
                        client: "c1".into(),
                        session: "$2".into(),
                        name: "claude/p/a".into(),
                        pane: "%3".into(),
                    },
                ),
                r#"{"id":14,"ok":{"client":"c1","session":"$2","name":"claude/p/a","pane":"%3"}}"#,
            ),
            (
                Response::ok(
                    15,
                    &ClientList {
                        clients: vec![client()],
                    },
                ),
                r#"{"id":15,"ok":{"clients":[{"id":"c1","session":"claude/p/a","pid":90}]}}"#,
            ),
            (
                Response::ok(
                    17,
                    &HostStatus {
                        sessions: 1,
                        clients: 0,
                        conpty: "bundled".into(),
                    },
                ),
                r#"{"id":17,"ok":{"sessions":1,"clients":0,"conpty":"bundled"}}"#,
            ),
            (Response::ok(11, &Done {}), r#"{"id":11,"ok":{}}"#),
            (
                Response::ok(
                    22,
                    &LaunchState {
                        session: "$2".into(),
                        pane: "%3".into(),
                        name: "claude/p/a".into(),
                        dead: true,
                        ended: true,
                        code: Some(126),
                        output: "hgs: claude: Access is denied. (os error 5)".into(),
                    },
                ),
                r#"{"id":22,"ok":{"session":"$2","pane":"%3","name":"claude/p/a","dead":true,"ended":true,"code":126,"output":"hgs: claude: Access is denied. (os error 5)"}}"#,
            ),
        ];
        for (response, text) in &cases {
            golden(response, text);
        }
        let errors = [
            (ErrorCode::NotFound, "not_found"),
            (ErrorCode::Exists, "exists"),
            (ErrorCode::Busy, "busy"),
            (ErrorCode::Unsupported, "unsupported"),
            (ErrorCode::Refused, "refused"),
            (ErrorCode::Invalid, "invalid"),
            (ErrorCode::Internal, "internal"),
        ];
        for (code, name) in errors {
            golden(
                &Response::error(8, code, "no session claude/p/x"),
                &format!(
                    r#"{{"id":8,"error":{{"code":"{name}","message":"no session claude/p/x"}}}}"#
                ),
            );
        }
    }

    /// Task #14 (P2.4): the change counters are optional both ways: a reply
    /// of a host that predates them reads as None, and a client that
    /// predates them (its own copy of the entries without the fields) reads
    /// a newer host's reply as before.
    #[test]
    fn change_counters_are_optional_for_old_hosts_and_old_clients() {
        let newer = r#"{"sessions":[],"panes":[{"session":"claude/p/a","session_id":"$2","pane":"%3","pid":77,"dead":false,"run_marker":"run-1","current_path":"C:\\src\\p","output":41}],"clients":[],"structure":7}"#;
        let listed: Listed = serde_json::from_str(newer).unwrap();
        assert_eq!((listed.structure, listed.panes[0].output), (Some(7), Some(41)));
        assert_eq!(serde_json::to_string(&listed).unwrap(), newer, "the fields round-trip");

        let older = r#"{"sessions":[],"panes":[{"session":"claude/p/a","session_id":"$2","pane":"%3","pid":77,"dead":false,"run_marker":"run-1","current_path":"C:\\src\\p"}],"clients":[]}"#;
        let listed: Listed = serde_json::from_str(older).unwrap();
        assert_eq!((listed.structure, listed.panes[0].output), (None, None));
        assert_eq!(serde_json::to_string(&listed).unwrap(), older, "an older host's reply is unchanged");

        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldPane {
            session: String,
            session_id: String,
            pane: String,
            pid: u32,
            dead: bool,
            run_marker: String,
            current_path: String,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldListed {
            sessions: Vec<SessionEntry>,
            panes: Vec<OldPane>,
            clients: Vec<ClientEntry>,
        }
        let old: OldListed = serde_json::from_str(newer).expect("an older client ignores the counters");
        assert_eq!(old.panes[0].run_marker, "run-1");
    }

    #[test]
    fn responses_decode_into_payloads_and_tolerate_newer_hosts() {
        let listed = Listed {
            sessions: Vec::new(),
            panes: Vec::new(),
            clients: Vec::new(),
            structure: None,
        };
        let response = Response::ok(2, &listed);
        assert_eq!(response.clone().payload::<Listed>(), Ok(listed));
        assert_eq!(
            response.payload::<Created>().unwrap_err().code,
            ErrorCode::Invalid
        );

        // Newer hosts add fields and error codes.
        let response: Response = serde_json::from_str(
            r#"{"id":4,"ok":{"sessions":1,"clients":2,"conpty":"inbox","recovery":true},"elapsed":3}"#,
        )
        .unwrap();
        assert_eq!(
            response.payload::<HostStatus>().unwrap(),
            HostStatus {
                sessions: 1,
                clients: 2,
                conpty: "inbox".into()
            }
        );
        let response: Response = serde_json::from_str(
            r#"{"id":4,"error":{"code":"quota","message":"later","retry":5}}"#,
        )
        .unwrap();
        let error = response.payload::<Done>().unwrap_err();
        assert_eq!(error.code, ErrorCode::Unknown);
        assert_eq!(error.to_string(), "later");

        // `ok` may be any JSON value; an error wins over it; one of them is required.
        let response: Response = serde_json::from_str(r#"{"id":1,"ok":null}"#).unwrap();
        assert_eq!(response.result, Ok(Value::Null));
        let response: Response = serde_json::from_str(
            r#"{"id":1,"ok":{},"error":{"code":"busy","message":"sessions run"}}"#,
        )
        .unwrap();
        assert_eq!(response.result.unwrap_err().code, ErrorCode::Busy);
        let response: Response = serde_json::from_str(r#"{"id":1,"ok":{},"error":null}"#).unwrap();
        assert_eq!(response.result, Ok(json!({})));
        for broken in [
            r#"{"id":1}"#,
            r#"{"ok":{}}"#,
            r#"{"id":1,"error":{"code":"busy"}}"#,
            "[]",
        ] {
            assert!(
                serde_json::from_str::<Response>(broken).is_err(),
                "{broken}"
            );
        }
        // The answer to an op without a payload is an empty object.
        assert_eq!(
            Response::ok(3, &Done::default()).payload::<Done>(),
            Ok(Done {})
        );
    }

    #[test]
    fn stream_control_has_stable_json() {
        golden(
            &Control::Resize(Size {
                cols: 100,
                rows: 30,
            }),
            r#"{"type":"resize","cols":100,"rows":30}"#,
        );
        golden(&Control::Detach, r#"{"type":"detach"}"#);
        golden(
            &Control::Exit { code: Some(0) },
            r#"{"type":"exit","code":0}"#,
        );
        golden(
            &Control::Notice {
                text: "Pasted".into(),
            },
            r#"{"type":"notice","text":"Pasted"}"#,
        );
        assert_eq!(
            serde_json::from_str::<Control>(r#"{"type":"exit"}"#).unwrap(),
            Control::Exit { code: None }
        );
        assert_eq!(
            serde_json::from_str::<Control>(r#"{"type":"title","text":"x"}"#).unwrap(),
            Control::Unknown
        );

        // Control and stream frames share a connection after attach.
        let mut bytes = Vec::new();
        write_json(&mut bytes, Kind::Control, &Control::Detach).unwrap();
        write_frame(&mut bytes, Kind::Stream, b"\x02d").unwrap();
        let mut reader = bytes.as_slice();
        let frame = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(frame.kind, Kind::Control);
        assert_eq!(frame.json::<Control>().unwrap(), Control::Detach);
        assert_eq!(read_frame(&mut reader).unwrap().unwrap().payload, b"\x02d");
    }
}
