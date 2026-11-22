//! The session host's client library (D3-D5, D13).
//!
//! `Connection` speaks protocol 1 over any `Link`: the hello, requests and
//! their answers, an attach stream and the supervisor registration. On Windows,
//! `connect` reaches a running host and never starts one, so a listing reports
//! no host as no live sessions, and only `ConnectError::NoHost` means that no
//! host runs; `connect_or_start` starts a host when none runs, outside this
//! process's job (D4), and retries while a host is starting or exiting (D3).
//! Both verify that the host that answers the hello is the process serving the
//! pipe; `connect_or_start` refuses a host that runs inside a job. Which
//! executable it starts is its caller's choice: the staged image (`gate`).
use super::{
    link::{write_json, Link},
    protocol::{
        Attach, Attached, Control, ErrorBody, ErrorCode, FrameError, Hello, HelloAnswer,
        HelloReply, Kind, Op, Request, Response, Role, Size, SupervisorRegistration, PROTOCOLS,
    },
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    fmt, io,
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

/// How long a client waits for the answer to its hello.
const HELLO_ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// Why there is no connection to a host.
#[derive(Debug)]
pub(crate) enum ClientError {
    /// No host runs (D3): the only case a listing may treat as no sessions.
    NoHost,
    /// The pipe could not be reached or verified.
    Connect(String),
    /// The host refused the hello.
    Refused(ErrorBody),
    /// The host closed the connection before it answered the hello.
    Closed,
    /// The host broke the protocol.
    Protocol(String),
    /// The host runs inside a job, so its sessions would end with that job (D4).
    InJob,
    /// No host could be started.
    Start(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::NoHost => f.write_str("no session host runs"),
            Self::Closed => f.write_str("the session host closed the connection during the hello"),
            Self::Connect(message) | Self::Protocol(message) | Self::Start(message) => {
                f.write_str(message)
            }
            Self::Refused(error) => f.write_str(&error.message),
            Self::InJob => f.write_str(
                "the session host runs inside a job that does not let it break away (for \
                 example an agent's sandbox), so its sessions would end with that job; start \
                 Zerus from the Start menu or Explorer",
            ),
        }
    }
}

impl std::error::Error for ClientError {}

fn lost(error: impl fmt::Display) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::Internal,
        format!("the session host connection failed: {error}"),
    )
}

/// A connection after an accepted hello.
pub(crate) struct Connection {
    link: Arc<dyn Link>,
    host: HelloReply,
    next: u64,
    /// Ends the connection at an observation's deadline ([`Self::hello_until`]).
    deadline: Option<Deadline>,
}

/// One absolute deadline over a connection (zerus-5sl7r): at `until` its link
/// is shut down, so a read or write still waiting then ends at once, however
/// the host spread its answer over reads that each came in time, and nothing
/// answers on it later. Dropped earlier, it ends without effect.
///
/// An answer that was read by then but is taken only after it (its read
/// completed as the link was shut down, or the reader ran late) is not taken
/// either: each answer on the connection is checked against `until` once its
/// JSON is decoded, before anything of it is returned ([`Self::passed`],
/// zerus-xngdd). A late answer is the connection's loss, never the host's
/// result, whatever it says.
struct Deadline {
    until: Instant,
    _disarm: mpsc::Sender<()>,
}

impl Deadline {
    fn arm(link: Arc<dyn Link>, until: Instant) -> io::Result<Self> {
        let (disarm, armed) = mpsc::channel::<()>();
        std::thread::Builder::new()
            .name("host-observation-deadline".into())
            .spawn(move || {
                let left = until.saturating_duration_since(Instant::now());
                if armed.recv_timeout(left) == Err(mpsc::RecvTimeoutError::Timeout) {
                    link.shutdown();
                }
            })?;
        Ok(Self {
            until,
            _disarm: disarm,
        })
    }

    /// Whether the deadline has passed: an answer decoded now is late.
    fn passed(&self) -> bool {
        now() >= self.until
    }
}

/// The time a deadline is checked against: the real clock, or in a test the
/// clock it controls ([`tests::clock`]).
fn now() -> Instant {
    #[cfg(test)]
    if let Some(now) = tests::clock::now() {
        return now;
    }
    Instant::now()
}

/// The answer to a request on a connection whose deadline passed before the
/// answer was taken.
fn late() -> ErrorBody {
    lost("the observation's deadline passed before its answer was taken")
}

impl Connection {
    /// Say hello as `role` and read the host's answer, waiting at most
    /// `HELLO_ANSWER_TIMEOUT` for it.
    pub(crate) fn hello(link: Arc<dyn Link>, role: Role) -> Result<Self, ClientError> {
        Self::hello_within(link, role, HELLO_ANSWER_TIMEOUT)
    }

    /// [`Self::hello`] for an observation that ends by `until`: the hello and
    /// every later request on this connection end then at the latest, the
    /// link shut down ([`Deadline`]).
    pub(crate) fn hello_until(
        link: Arc<dyn Link>,
        role: Role,
        until: Instant,
    ) -> Result<Self, ClientError> {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(ClientError::Connect(
                "the observation's deadline passed".into(),
            ));
        }
        let deadline = Deadline::arm(link.clone(), until).map_err(|error| {
            ClientError::Connect(format!("the observation's deadline: {error}"))
        })?;
        let mut connection = Self::hello_within(link, role, left)?;
        // The hello answer, read and decoded, counts only by the deadline.
        if deadline.passed() {
            return Err(ClientError::Connect(
                "the observation's deadline passed before its hello answer was taken".into(),
            ));
        }
        connection.deadline = Some(deadline);
        Ok(connection)
    }

    /// [`Self::hello`], waiting at most `wait` for the answer.
    pub(crate) fn hello_within(
        link: Arc<dyn Link>,
        role: Role,
        wait: Duration,
    ) -> Result<Self, ClientError> {
        let early = |error: FrameError| match error {
            FrameError::Io(_) | FrameError::Truncated => ClientError::Closed,
            other => ClientError::Protocol(format!("invalid session host hello answer: {other}")),
        };
        let unusable = |error: io::Error| {
            ClientError::Connect(format!("the session host connection failed: {error}"))
        };
        link.set_read_timeout(Some(wait.max(Duration::from_millis(1))))
            .map_err(unusable)?;
        write_json(&*link, Kind::Json, &Hello::new(role)).map_err(early)?;
        let frame = link
            .read_frame()
            .map_err(early)?
            .ok_or(ClientError::Closed)?;
        link.set_read_timeout(None).map_err(unusable)?;
        if frame.kind != Kind::Json {
            return Err(ClientError::Protocol(
                "the session host answered the hello with stream data".into(),
            ));
        }
        match frame.json::<HelloAnswer>() {
            // The host chooses from the range this hello offered; any other
            // major is one this build cannot speak.
            Ok(HelloAnswer::Accepted(host))
                if !(PROTOCOLS.min..=PROTOCOLS.max).contains(&host.protocol) =>
            {
                Err(ClientError::Protocol(format!(
                    "the session host chose protocol {}, which hgs {} does not speak",
                    host.protocol,
                    env!("CARGO_PKG_VERSION")
                )))
            }
            Ok(HelloAnswer::Accepted(host)) => Ok(Self {
                link,
                host,
                next: 1,
                deadline: None,
            }),
            Ok(HelloAnswer::Refused(error)) => Err(ClientError::Refused(error)),
            Err(error) => Err(ClientError::Protocol(format!(
                "invalid session host hello answer: {error}"
            ))),
        }
    }

    /// The host's identity from its hello answer.
    pub(crate) fn host(&self) -> &HelloReply {
        &self.host
    }

    /// How long a request waits for its answer; None (the default) waits for
    /// ever. A request that timed out leaves the connection out of step.
    pub(crate) fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.link.set_read_timeout(timeout)
    }

    /// Send `op` and wait for its answer.
    pub(crate) fn request(&mut self, op: Op) -> Result<Value, ErrorBody> {
        let id = self.next;
        self.next += 1;
        write_json(&*self.link, Kind::Json, &Request { id, op }).map_err(lost)?;
        let frame = self
            .link
            .read_frame()
            .map_err(lost)?
            .ok_or_else(|| lost("the host closed the connection"))?;
        if frame.kind != Kind::Json {
            return Err(lost("the host answered with stream data"));
        }
        #[cfg(test)]
        tests::clock::before_json();
        let response: Response = frame.json().map_err(lost)?;
        #[cfg(test)]
        tests::clock::after_json();
        if response.id != id {
            return Err(lost(format!(
                "the answer to request {id} came as {}",
                response.id
            )));
        }
        // An answer, however it was read and decoded, counts only by the
        // deadline: a late one is the connection's loss, its result unread.
        if self.deadline.as_ref().is_some_and(Deadline::passed) {
            return Err(late());
        }
        response.result
    }

    /// `request` with the answer's payload as `T`.
    pub(crate) fn call<T: DeserializeOwned>(&mut self, op: Op) -> Result<T, ErrorBody> {
        Response {
            id: 0,
            result: self.request(op),
        }
        .payload()
    }

    /// Attach to a session; the connection then carries its stream.
    pub(crate) fn attach(mut self, attach: Attach) -> Result<(Attached, AttachStream), ErrorBody> {
        let attached = self.call(Op::Attach(attach))?;
        Ok((attached, AttachStream { link: self.link }))
    }

    /// Offer this connection as a session's supervisor channel (D10); answer
    /// the host's requests on the returned link with `channel::answer_requests`.
    pub(crate) fn register_supervisor(
        mut self,
        registration: SupervisorRegistration,
    ) -> Result<Arc<dyn Link>, ErrorBody> {
        self.request(Op::SupervisorRegister(registration))?;
        Ok(self.link)
    }
}

/// What an attached client receives.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Output(Vec<u8>),
    Control(Control),
}

/// An attached connection (D13). One thread may wait in `next` while another
/// sends.
#[derive(Clone)]
pub(crate) struct AttachStream {
    link: Arc<dyn Link>,
}

impl AttachStream {
    /// Bytes typed in the client's terminal.
    pub(crate) fn send(&self, bytes: &[u8]) -> Result<(), FrameError> {
        self.link.write_frame(Kind::Stream, bytes)
    }

    /// The client's terminal has a new size.
    pub(crate) fn resize(&self, size: Size) -> Result<(), FrameError> {
        write_json(&*self.link, Kind::Control, &Control::Resize(size))
    }

    /// Leave the session; it runs on. The host then closes the connection.
    pub(crate) fn detach(&self) -> Result<(), FrameError> {
        write_json(&*self.link, Kind::Control, &Control::Detach)
    }

    /// End this side of the connection: a thread waiting in `next` gets the
    /// end, and later sends fail.
    pub(crate) fn close(&self) {
        self.link.shutdown();
    }

    /// The next output or control message; None once the host closed the
    /// connection (after `Control::Exit`, a detach, or the host's death).
    pub(crate) fn next(&self) -> Result<Option<Event>, FrameError> {
        loop {
            let Some(frame) = self.link.read_frame()? else {
                return Ok(None);
            };
            match frame.kind {
                Kind::Stream => return Ok(Some(Event::Output(frame.payload))),
                Kind::Control => match frame.json::<Control>() {
                    Ok(Control::Unknown) => {}
                    Ok(control) => return Ok(Some(Event::Control(control))),
                    Err(error) => return Err(std::io::Error::from(error).into()),
                },
                Kind::Json => {}
            }
        }
    }
}

#[cfg(windows)]
pub(crate) use self::windows::{connect, connect_or_start, connect_until};

#[cfg(windows)]
mod windows {
    use super::{
        super::{
            endpoint, gate,
            log::LOG_FILE,
            pipe::{self, ConnectError},
            server::{PipeLink, EXIT_OTHER_HOST},
            spawn::{spawn_detached, Child, Command, Environment, Flags, LaunchError},
            token,
        },
        *,
    };
    use std::{
        path::{Path, PathBuf},
        thread,
        time::Instant,
    };
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND};

    /// How long one connect waits while every pipe instance is busy.
    const BUSY_WAIT: Duration = Duration::from_secs(5);
    /// How long `connect_or_start` keeps trying.
    const START_DEADLINE: Duration = Duration::from_secs(20);

    /// What one attempt found.
    enum Attempt {
        Connected(Connection),
        /// Nothing serves the pipe (D3: the only "no host").
        Absent,
        /// The host closed the connection before its hello answer: it is
        /// exiting, and gone soon.
        Closing,
        /// Every pipe instance stayed busy (an exiting host keeps its connected
        /// instances while it stops listening).
        Busy,
        Failed(ClientError),
    }

    fn attempt(state_dir: &Path, role: Role, deadline: Instant) -> Attempt {
        attempt_within(state_dir, role, deadline, None)
    }

    /// [`attempt`]; with `observation`, the connection's hello and every
    /// later request end by that deadline ([`Connection::hello_until`]).
    fn attempt_within(
        state_dir: &Path,
        role: Role,
        deadline: Instant,
        observation: Option<Instant>,
    ) -> Attempt {
        let secret = match endpoint::existing_secret(state_dir) {
            Ok(Some(secret)) => secret,
            Ok(None) => return Attempt::Absent,
            Err(error) => return Attempt::Failed(ClientError::Connect(error.to_string())),
        };
        let sid = match token::user_sid() {
            Ok(sid) => sid,
            Err(error) => return Attempt::Failed(ClientError::Connect(error.to_string())),
        };
        let name = endpoint::pipe_name(&sid, state_dir, &secret);
        let stream = match pipe::connect(&name, deadline) {
            Ok(stream) => stream,
            Err(ConnectError::NoHost) => return Attempt::Absent,
            Err(ConnectError::Busy) => return Attempt::Busy,
            Err(error) => return Attempt::Failed(ClientError::Connect(error.to_string())),
        };
        let server = match stream.server_pid() {
            Ok(pid) => pid,
            Err(error) => return Attempt::Failed(ClientError::Connect(error.to_string())),
        };
        let link: Arc<dyn Link> = Arc::new(PipeLink::new(stream));
        let hello = match observation {
            Some(until) => Connection::hello_until(link, role, until),
            None => Connection::hello(link, role),
        };
        match hello {
            Err(ClientError::Closed) => Attempt::Closing,
            Err(error) => Attempt::Failed(error),
            Ok(connection) if connection.host().pid != server => {
                Attempt::Failed(ClientError::Connect(format!(
                    "the session host answered as process {} but process {server} serves its pipe",
                    connection.host().pid
                )))
            }
            Ok(connection) => Attempt::Connected(connection),
        }
    }

    /// Connect to the host of the canonical `state_dir`; never starts one. A
    /// host that is exiting is waited for until it is gone.
    pub(crate) fn connect(state_dir: &Path, role: Role) -> Result<Connection, ClientError> {
        let deadline = Instant::now() + BUSY_WAIT;
        loop {
            match attempt(state_dir, role, deadline) {
                Attempt::Connected(connection) => return Ok(connection),
                Attempt::Absent => return Err(ClientError::NoHost),
                Attempt::Closing if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(50))
                }
                Attempt::Closing => return Err(ClientError::Closed),
                Attempt::Busy => return Err(ClientError::Connect(ConnectError::Busy.to_string())),
                Attempt::Failed(error) => return Err(error),
            }
        }
    }

    /// One connect to the host of the canonical `state_dir` for an
    /// observation that ends by `until` (zerus-5sl7r): the pipe by then, and
    /// the hello and every request on the connection by then too, whatever
    /// the pipe's wait took ([`Connection::hello_until`]). A busy pipe or an
    /// exiting host is not waited for, and none is ever started.
    pub(crate) fn connect_until(
        state_dir: &Path,
        role: Role,
        until: Instant,
    ) -> Result<Connection, ClientError> {
        if Instant::now() >= until {
            return Err(ClientError::Connect(
                "the observation's deadline passed".into(),
            ));
        }
        match attempt_within(state_dir, role, until, Some(until)) {
            Attempt::Connected(connection) => Ok(connection),
            Attempt::Absent => Err(ClientError::NoHost),
            Attempt::Closing => Err(ClientError::Closed),
            Attempt::Busy => Err(ClientError::Connect(ConnectError::Busy.to_string())),
            Attempt::Failed(error) => Err(error),
        }
    }

    /// Why `start` started nothing.
    enum StartError {
        /// The image is gone: a later install retired it (D2).
        Missing(ClientError),
        Failed(ClientError),
    }

    /// Start `image` as the host of `state_dir` (D4): outside this process's
    /// job, hidden, in its own process group, in the state directory.
    fn start(state_dir: &Path, image: &Path) -> Result<Child, StartError> {
        let failed = |error: &dyn fmt::Display| {
            StartError::Failed(ClientError::Start(format!(
                "cannot start the session host: {error}"
            )))
        };
        crate::self_exec::ensure().map_err(|error| failed(&error))?;
        let mut environment = Environment::current_with_optional_hint_controls()
            .map_err(|e| failed(&e))?;
        let state = state_dir
            .to_str()
            .ok_or_else(|| failed(&"the state directory is not Unicode"))?;
        environment
            .set("HGS_STATE_DIR", state)
            .map_err(|e| failed(&e))?;
        let argv = vec![
            image.display().to_string(),
            "__host".to_owned(),
            "serve".to_owned(),
        ];
        spawn_detached(&Command {
            application: image,
            argv: &argv,
            environment: &environment,
            directory: Some(state_dir),
            flags: Flags::default(),
        })
        .map_err(|error| match &error {
            LaunchError::Spawn(io)
                if [ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND]
                    .map(|code| Some(code as i32))
                    .contains(&io.raw_os_error()) =>
            {
                StartError::Missing(ClientError::Start(format!(
                    "cannot start the session host {}: {error}",
                    image.display()
                )))
            }
            _ => failed(&error),
        })
    }

    /// Connect to the host of the canonical `state_dir`, starting the
    /// executable `image` gives as the host when none runs. `image` is asked
    /// again before each start, so a launcher that read the pointer of an image
    /// a later install retired starts the new one (D2). A host that is still
    /// starting, or one that is exiting and holds the pipe or the lock, is
    /// waited for. A host that refuses to serve (`gate`) is reported.
    pub(crate) fn connect_or_start(
        state_dir: &Path,
        role: Role,
        image: &dyn Fn() -> Result<PathBuf, ClientError>,
    ) -> Result<Connection, ClientError> {
        let deadline = Instant::now() + START_DEADLINE;
        let mut started: Option<Child> = None;
        // The image whose start found it missing.
        let mut missing: Option<PathBuf> = None;
        loop {
            let absent = match attempt(state_dir, role, deadline.min(Instant::now() + BUSY_WAIT)) {
                // A host that shares the desktop shell's job ends no earlier
                // than the desktop does (D4).
                Attempt::Connected(connection)
                    if connection.host().in_job && !connection.host().desktop_job =>
                {
                    return Err(ClientError::InJob)
                }
                Attempt::Connected(connection) => return Ok(connection),
                Attempt::Failed(error) => return Err(error),
                // An exiting host goes away first; its successor starts then.
                Attempt::Closing | Attempt::Busy => false,
                Attempt::Absent => true,
            };
            if Instant::now() >= deadline {
                return Err(ClientError::Start(
                    "the session host did not start in time".into(),
                ));
            }
            if !absent {
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            // WhiteWh fork: an SSH session (a Windows peer, src/windows_peer.rs)
            // never starts the host: one started there would carry that
            // session's token (elevated for an administrator, which then shuts
            // the desktop out) and end with it. Zerus on this computer runs it.
            if started.is_none() && std::env::var_os("SSH_CONNECTION").is_some() {
                return Err(ClientError::Start(
                    "the session host of this computer is not running: start Zerus on it".into(),
                ));
            }
            // Start a host unless the one started last is still coming up. A
            // host that found another one holding the lock or the pipe exits
            // with EXIT_OTHER_HOST: that one is exiting, so try again.
            let running = match started.as_ref().map(|child| child.wait(Duration::ZERO)) {
                Some(Ok(None)) => true,
                Some(Ok(Some(code))) if code as i32 != EXIT_OTHER_HOST => {
                    if let Some(refusal) = gate::launch_refusal(code as i32) {
                        return Err(ClientError::Start(format!(
                            "the session host refused to start: {refusal}"
                        )));
                    }
                    return Err(ClientError::Start(format!(
                        "the session host exited with {code:#x} while starting; see {} in {}",
                        LOG_FILE,
                        state_dir.display()
                    )));
                }
                Some(Ok(Some(_))) => {
                    thread::sleep(Duration::from_millis(200));
                    false
                }
                Some(Err(_)) | None => false,
            };
            if !running {
                let image = image()?;
                match start(state_dir, &image) {
                    Ok(child) => started = Some(child),
                    // Read the pointer again, once per image that went missing.
                    Err(StartError::Missing(_)) if missing.as_ref() != Some(&image) => {
                        missing = Some(image);
                        continue;
                    }
                    Err(StartError::Missing(error) | StartError::Failed(error)) => {
                        return Err(error)
                    }
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{link::memory_pair, protocol::Integrity};
    use super::*;
    use std::thread;

    /// A clock the deadline tests control, on their own thread only
    /// (zerus-xngdd, the review of #2538): the time an answer is read
    /// ([`now`]), a pause before its JSON is decoded and the time that decoding
    /// ends.
    pub(super) mod clock {
        use std::{cell::Cell, time::Instant};

        thread_local! {
            static NOW: Cell<Option<Instant>> = const { Cell::new(None) };
            static PAUSE_UNTIL: Cell<Option<Instant>> = const { Cell::new(None) };
            static DECODED_AT: Cell<Option<Instant>> = const { Cell::new(None) };
        }

        /// The controlled time, when a test set one.
        pub(in super::super) fn now() -> Option<Instant> {
            NOW.with(Cell::get)
        }
        pub(in super::super) fn set(now: Option<Instant>) {
            NOW.set(now);
        }
        /// Before the next answer's JSON is decoded, wait in real time until
        /// `until`, as a reader scheduled late would.
        pub(in super::super) fn pause_before_json(until: Instant) {
            PAUSE_UNTIL.set(Some(until));
        }
        /// The next answer's JSON decoding ends at `at`.
        pub(in super::super) fn decoded_at(at: Instant) {
            DECODED_AT.set(Some(at));
        }
        pub(in super::super) fn before_json() {
            if let Some(until) = PAUSE_UNTIL.take() {
                while Instant::now() < until {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        }
        pub(in super::super) fn after_json() {
            if let Some(at) = DECODED_AT.take() {
                NOW.set(Some(at));
            }
        }
    }

    fn identity() -> HelloReply {
        HelloReply {
            pid: 7,
            created: "1".into(),
            image: "hgs".into(),
            image_hash: "00".into(),
            version: "1".into(),
            protocol: 1,
            capabilities: Vec::new(),
            integrity: Integrity::from_rid(Integrity::MEDIUM),
            windows_session: 1,
            in_job: false,
            desktop_job: false,
        }
    }

    #[test]
    fn a_refused_or_broken_hello_is_reported() {
        let (client, host) = memory_pair();
        let server = thread::spawn(move || {
            host.read_frame().unwrap().unwrap();
            write_json(
                &host,
                Kind::Json,
                &HelloAnswer::Refused(ErrorBody::new(ErrorCode::Refused, "elevated")),
            )
            .unwrap();
        });
        let error = Connection::hello(Arc::new(client), Role::Cli)
            .err()
            .unwrap();
        assert!(matches!(&error, ClientError::Refused(body) if body.message == "elevated"));
        server.join().unwrap();

        let (client, host) = memory_pair();
        drop(host);
        assert!(matches!(
            Connection::hello(Arc::new(client), Role::Cli),
            Err(ClientError::Closed)
        ));

        // Answers must match their requests.
        let (client, host) = memory_pair();
        let server = thread::spawn(move || {
            host.read_frame().unwrap().unwrap();
            write_json(&host, Kind::Json, &HelloAnswer::Accepted(identity())).unwrap();
            host.read_frame().unwrap().unwrap();
            write_json(&host, Kind::Json, &Response::ok(9, &serde_json::json!({}))).unwrap();
            host.read_frame().unwrap();
        });
        let mut connection = Connection::hello(Arc::new(client), Role::Cli).unwrap();
        assert_eq!(connection.host().pid, 7);
        let error = connection.request(Op::List).unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal);
        assert!(error.message.contains("came as 9"), "{}", error.message);
        drop(connection);
        server.join().unwrap();
        assert_eq!(ClientError::NoHost.to_string(), "no session host runs");
    }

    /// zerus-5sl7r: an observation's connection ends by its own deadline: a
    /// host that takes the hello but never answers it, or a request it never
    /// answers, ends the wait at the deadline, not at the hello's usual 10 s
    /// or never.
    #[test]
    fn an_observation_waits_for_a_silent_host_only_until_its_deadline() {
        let wait = Duration::from_millis(300);
        let (client, host) = memory_pair();
        let held = thread::spawn(move || {
            host.read_frame().unwrap().unwrap();
            thread::sleep(Duration::from_secs(3));
        });
        let started = std::time::Instant::now();
        assert!(Connection::hello_within(Arc::new(client), Role::Cli, wait).is_err());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        held.join().unwrap();

        let (client, host) = memory_pair();
        let held = thread::spawn(move || {
            host.read_frame().unwrap().unwrap();
            write_json(&host, Kind::Json, &HelloAnswer::Accepted(identity())).unwrap();
            host.read_frame().unwrap().unwrap();
            thread::sleep(Duration::from_secs(3));
        });
        let mut connection = Connection::hello_within(Arc::new(client), Role::Cli, wait).unwrap();
        connection.set_read_timeout(Some(wait)).unwrap();
        let started = std::time::Instant::now();
        assert!(connection.request(Op::List).is_err());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        drop(connection);
        held.join().unwrap();
    }

    /// zerus-5sl7r, the review's fragmented-frame probe: a host that sends
    /// its hello answer, or the answer to a request, in pieces that each come
    /// within a read's timeout still ends the observation at its one deadline
    /// (300 ms here), not at about 500 ms with a late answer taken.
    #[cfg(unix)]
    #[test]
    fn an_observation_ends_at_its_deadline_however_the_host_spreads_its_answer() {
        use std::{io::Write, net::Shutdown, os::unix::net::UnixStream};
        struct StreamLink(UnixStream);
        impl Link for StreamLink {
            fn read_frame(&self) -> Result<Option<super::super::protocol::Frame>, FrameError> {
                super::super::protocol::read_frame(&mut &self.0)
            }
            fn write_frame(&self, kind: Kind, payload: &[u8]) -> Result<(), FrameError> {
                super::super::protocol::write_frame(&mut &self.0, kind, payload)
            }
            fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
                self.0.set_read_timeout(timeout)
            }
            fn shutdown(&self) {
                let _ = self.0.shutdown(Shutdown::Both);
            }
        }
        // Each piece in time for one read's timeout, the whole one late.
        fn drip(stream: &mut UnixStream, payload: &[u8]) {
            for part in [&payload[..4], &payload[4..5], &payload[5..]] {
                thread::sleep(Duration::from_millis(165));
                if stream.write_all(part).is_err() {
                    return;
                }
            }
        }
        fn framed<T: serde::Serialize>(value: &T) -> Vec<u8> {
            let mut bytes = Vec::new();
            super::super::protocol::write_json(&mut bytes, Kind::Json, value).unwrap();
            bytes
        }
        let wait = Duration::from_millis(300);
        let host = |answer_hello_at_once: bool| {
            let (client, mut peer) = UnixStream::pair().unwrap();
            let server = thread::spawn(move || {
                super::super::protocol::read_frame(&mut peer)
                    .unwrap()
                    .unwrap();
                let hello = framed(&HelloAnswer::Accepted(identity()));
                if answer_hello_at_once {
                    peer.write_all(&hello).unwrap();
                } else {
                    drip(&mut peer, &hello);
                    return;
                }
                if let Ok(Some(_)) = super::super::protocol::read_frame(&mut peer) {
                    drip(&mut peer, &framed(&Response::ok(1, &serde_json::json!({}))));
                }
            });
            (Arc::new(StreamLink(client)) as Arc<dyn Link>, server)
        };
        // The hello answer spread over 495 ms.
        let (link, server) = host(false);
        let started = Instant::now();
        assert!(Connection::hello_until(link, Role::Cli, started + wait).is_err());
        let hello = started.elapsed();
        server.join().unwrap();
        // The hello answered at once, the request's answer spread over 495 ms.
        let (link, server) = host(true);
        let started = Instant::now();
        let mut connection = Connection::hello_until(link, Role::Cli, started + wait).unwrap();
        connection.set_read_timeout(Some(wait)).unwrap();
        assert!(connection.request(Op::List).is_err());
        let request = started.elapsed();
        drop(connection);
        server.join().unwrap();
        println!(
            "300 ms deadline: spread hello ended after {hello:?}, spread answer after {request:?}"
        );
        assert!(hello < Duration::from_millis(450), "{hello:?}");
        assert!(request < Duration::from_millis(450), "{request:?}");
        // A prompt host is answered as before, and the deadline then ends
        // nothing.
        let (client, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            super::super::protocol::read_frame(&mut peer)
                .unwrap()
                .unwrap();
            peer.write_all(&framed(&HelloAnswer::Accepted(identity())))
                .unwrap();
            super::super::protocol::read_frame(&mut peer)
                .unwrap()
                .unwrap();
            peer.write_all(&framed(&Response::ok(1, &serde_json::json!({"ok": true}))))
                .unwrap();
        });
        let link: Arc<dyn Link> = Arc::new(StreamLink(client));
        let mut connection =
            Connection::hello_until(link, Role::Cli, Instant::now() + wait).unwrap();
        assert_eq!(
            connection.request(Op::List).unwrap(),
            serde_json::json!({"ok": true})
        );
        server.join().unwrap();
    }

    /// zerus-xngdd, the review's buffered-frame probe over a Unix socket
    /// pair with the production frame decoder: a frame read by the deadline
    /// but taken only after it is refused, for the hello and for a request.
    /// The reader here holds a frame it read until the deadline has shut the
    /// link, as a pipe read that completes while its cancellation runs. The
    /// probe's other stages stay as they were: a host that spreads its answer
    /// past the deadline ends there, and one that spreads it within the
    /// deadline is answered (a 1 s budget for about 400 ms of pieces).
    #[cfg(unix)]
    #[test]
    fn a_frame_taken_after_the_deadline_is_refused_though_it_was_read_in_time() {
        use std::{
            io::Write,
            net::Shutdown,
            os::unix::net::UnixStream,
            sync::atomic::{AtomicBool, AtomicU64, Ordering},
        };
        struct Held {
            io: UnixStream,
            shut: Arc<AtomicBool>,
            /// The deadline and the read whose frame is held until the link
            /// is shut.
            hold: Option<(Instant, u64)>,
            reads: AtomicU64,
        }
        impl Link for Held {
            fn read_frame(&self) -> Result<Option<super::super::protocol::Frame>, FrameError> {
                let n = self.reads.fetch_add(1, Ordering::Relaxed);
                let frame = super::super::protocol::read_frame(&mut &self.io)?;
                if let Some((until, nth)) = self.hold {
                    if frame.is_some() && n == nth {
                        let ceiling = until + Duration::from_millis(500);
                        while !self.shut.load(Ordering::Acquire) && Instant::now() < ceiling {
                            thread::sleep(Duration::from_millis(2));
                        }
                        assert!(self.shut.load(Ordering::Acquire));
                        assert!(Instant::now() >= until);
                    }
                }
                Ok(frame)
            }
            fn write_frame(&self, kind: Kind, payload: &[u8]) -> Result<(), FrameError> {
                super::super::protocol::write_frame(&mut &self.io, kind, payload)
            }
            fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
                self.io.set_read_timeout(timeout)
            }
            fn shutdown(&self) {
                let _ = self.io.shutdown(Shutdown::Both);
                self.shut.store(true, Ordering::Release);
            }
        }
        fn pieces(peer: &mut UnixStream, answer: &impl serde::Serialize, pace: Duration) -> bool {
            let mut bytes = Vec::new();
            super::super::protocol::write_json(&mut bytes, Kind::Json, answer).unwrap();
            for part in [&bytes[..4], &bytes[4..5], &bytes[5..]] {
                thread::sleep(pace);
                if peer.write_all(part).is_err() {
                    return false;
                }
            }
            true
        }
        for (stage, hello_pace, answer_pace, budget, hold) in [
            ("late fragmented hello", 165, 0, 300, None),
            ("late fragmented answer", 0, 165, 300, None),
            ("legitimate fragmented round trip", 63, 68, 1000, None),
            (
                "buffered hello taken after the deadline",
                0,
                0,
                230,
                Some(0),
            ),
            (
                "buffered answer taken after the deadline",
                0,
                0,
                230,
                Some(1),
            ),
        ] {
            let (client, mut peer) = UnixStream::pair().unwrap();
            let shut = Arc::new(AtomicBool::new(false));
            let server = thread::spawn(move || {
                if !matches!(super::super::protocol::read_frame(&mut peer), Ok(Some(_))) {
                    return;
                }
                let hello = HelloAnswer::Accepted(identity());
                if pieces(&mut peer, &hello, Duration::from_millis(hello_pace))
                    && matches!(super::super::protocol::read_frame(&mut peer), Ok(Some(_)))
                {
                    let answer = Response::ok(1, &serde_json::json!({"system": "fixture"}));
                    pieces(&mut peer, &answer, Duration::from_millis(answer_pace));
                }
            });
            let started = Instant::now();
            let until = started + Duration::from_millis(budget);
            let link = Held {
                io: client,
                shut: shut.clone(),
                hold: hold.map(|nth| (until, nth)),
                reads: AtomicU64::new(0),
            };
            let hello = Connection::hello_until(Arc::new(link), Role::Cli, until);
            let hello_ok = hello.is_ok();
            let mut answered = None;
            if let Ok(mut connection) = hello {
                let left = until.saturating_duration_since(Instant::now());
                if !left.is_zero() {
                    connection.set_read_timeout(Some(left)).unwrap();
                    answered = Some(connection.request(Op::List).is_ok());
                }
            }
            let elapsed = started.elapsed();
            let shut = shut.load(Ordering::Acquire);
            println!(
                "{stage}: budget {budget} ms, hello {hello_ok}, answer {answered:?}, ended after {elapsed:?}, link shut {shut}"
            );
            let within = |low: u64, high: u64| {
                elapsed >= Duration::from_millis(low) && elapsed < Duration::from_millis(high)
            };
            match stage {
                "late fragmented hello" => assert!(!hello_ok && within(280, 455), "{stage}"),
                "late fragmented answer" => {
                    assert!(
                        hello_ok && answered == Some(false) && within(280, 455),
                        "{stage}"
                    )
                }
                "legitimate fragmented round trip" => {
                    assert!(
                        hello_ok && answered == Some(true) && within(350, budget),
                        "{stage}"
                    )
                }
                "buffered hello taken after the deadline" => {
                    assert!(!hello_ok && answered.is_none() && shut, "{stage}")
                }
                _ => assert!(hello_ok && answered == Some(false) && shut, "{stage}"),
            }
            server.join().unwrap();
        }
    }

    /// The review of #2538's controlled-clock cases (zerus-xngdd): an answer
    /// counts only if its decoding ends before the deadline, whatever it says.
    /// The link hands over whole frames and sets the controlled clock to the
    /// time each is read; the clock may then move while its JSON is decoded.
    mod controlled_clock {
        use super::super::super::protocol::{Frame, Target};
        use super::super::*;
        use super::{clock, identity};
        use std::sync::{
            atomic::{AtomicU64, Ordering},
            Mutex,
        };

        fn frame(value: &impl serde::Serialize) -> Frame {
            let mut wire = Vec::new();
            super::super::super::protocol::write_json(&mut wire, Kind::Json, value).unwrap();
            super::super::super::protocol::read_frame(&mut wire.as_slice())
                .unwrap()
                .unwrap()
        }
        fn observation() -> Op {
            Op::PaneInfo {
                target: Target::Pane("%4".into()),
            }
        }
        fn found() -> Response {
            Response::ok(
                1,
                &serde_json::json!({
                    "session_id": "$2", "session_name": "fixture", "pane": "%4",
                    "server_pid": 7, "pane_pid": 8, "width": 80, "height": 30,
                    "cursor_x": 0, "cursor_y": 0, "in_mode": false,
                    "current_command": "fixture", "dead": false, "run_marker": "",
                    "bracketed_paste": true, "tty": "conpty:%4"
                }),
            )
        }
        fn missing() -> Response {
            Response {
                id: 1,
                result: Err(ErrorBody::new(ErrorCode::NotFound, "fixture pane missing")),
            }
        }

        /// A host's hello answer and one reply, each read at a given time.
        struct Timed {
            hello: Mutex<Option<Frame>>,
            reply: Mutex<Option<Frame>>,
            hello_at: Option<Instant>,
            reply_at: Option<Instant>,
        }
        impl Link for Timed {
            fn read_frame(&self) -> Result<Option<Frame>, FrameError> {
                if let Some(hello) = self.hello.lock().unwrap().take() {
                    clock::set(self.hello_at);
                    return Ok(Some(hello));
                }
                clock::set(self.reply_at);
                Ok(self.reply.lock().unwrap().take())
            }
            fn write_frame(&self, _: Kind, _: &[u8]) -> Result<(), FrameError> {
                Ok(())
            }
            fn set_read_timeout(&self, _: Option<Duration>) -> io::Result<()> {
                Ok(())
            }
            fn shutdown(&self) {}
        }
        fn host(
            reply: Response,
            hello_at: Option<Instant>,
            reply_at: Option<Instant>,
        ) -> Arc<dyn Link> {
            Arc::new(Timed {
                hello: Mutex::new(Some(frame(&HelloAnswer::Accepted(identity())))),
                reply: Mutex::new(Some(frame(&reply))),
                hello_at,
                reply_at,
            })
        }
        fn is_late(error: &ErrorBody) -> bool {
            error.code == ErrorCode::Internal && error.message.contains("deadline passed")
        }

        /// The review's three cases: a reply whose decoding ends at the
        /// deadline, a host's NotFound or its success, or one decoded only
        /// after a scheduling pause there, is the connection's loss.
        #[test]
        fn an_answer_decoded_at_or_after_the_deadline_is_lost_whatever_it_says() {
            let until = Instant::now() + Duration::from_secs(5);
            let near = Some(until - Duration::from_nanos(1));
            for reply in [missing(), found()] {
                let mut connection =
                    Connection::hello_until(host(reply, near, near), Role::Cli, until).unwrap();
                clock::decoded_at(until);
                let result = connection.request(observation());
                assert_eq!(clock::now(), Some(until));
                assert!(result.as_ref().is_err_and(is_late), "{result:?}");
            }
            clock::set(None);
            let until = Instant::now() + Duration::from_millis(300);
            let mut connection =
                Connection::hello_until(host(missing(), None, None), Role::Cli, until).unwrap();
            clock::pause_before_json(until);
            let result = connection.request(observation());
            assert!(Instant::now() >= until);
            assert!(result.as_ref().is_err_and(is_late), "{result:?}");
        }

        /// The review's boundary controls: an answer read at the deadline is
        /// lost, a hello answer read then is refused, and one decoded a
        /// nanosecond before it is the host's own result.
        #[test]
        fn the_deadline_is_the_boundary() {
            let until = Instant::now() + Duration::from_secs(5);
            let near = Some(until - Duration::from_nanos(1));
            for reply in [found(), missing()] {
                let mut connection =
                    Connection::hello_until(host(reply, near, Some(until)), Role::Cli, until)
                        .unwrap();
                let result = connection.request(observation());
                assert!(result.as_ref().is_err_and(is_late), "{result:?}");
            }
            let hello = Connection::hello_until(host(found(), Some(until), None), Role::Cli, until);
            assert!(
                matches!(&hello, Err(ClientError::Connect(message)) if message.contains("deadline passed")),
                "{:?}",
                hello.as_ref().err()
            );
            let mut live =
                Connection::hello_until(host(found(), near, near), Role::Cli, until).unwrap();
            assert_eq!(live.request(observation()).unwrap()["pane"], "%4");
            let mut gone =
                Connection::hello_until(host(missing(), near, near), Role::Cli, until).unwrap();
            assert_eq!(
                gone.request(observation()).unwrap_err().code,
                ErrorCode::NotFound
            );
            clock::set(None);
        }

        /// A connection without a deadline (every use but an observation)
        /// keeps a late NotFound and the answers after it (the review's
        /// ordinary-connection control).
        #[test]
        fn a_connection_without_a_deadline_keeps_late_answers() {
            struct Ordinary(AtomicU64);
            impl Link for Ordinary {
                fn read_frame(&self) -> Result<Option<Frame>, FrameError> {
                    Ok(Some(match self.0.fetch_add(1, Ordering::Relaxed) {
                        0 => frame(&HelloAnswer::Accepted(identity())),
                        1 => {
                            std::thread::sleep(Duration::from_millis(25));
                            frame(&missing())
                        }
                        n => frame(&Response::ok(n, &serde_json::json!({"fixture": 12}))),
                    }))
                }
                fn write_frame(&self, _: Kind, _: &[u8]) -> Result<(), FrameError> {
                    Ok(())
                }
                fn set_read_timeout(&self, _: Option<Duration>) -> io::Result<()> {
                    Ok(())
                }
                fn shutdown(&self) {}
            }
            let mut connection =
                Connection::hello(Arc::new(Ordinary(AtomicU64::new(0))), Role::Cli).unwrap();
            assert_eq!(
                connection.request(observation()).unwrap_err().code,
                ErrorCode::NotFound
            );
            for _ in 0..3 {
                assert_eq!(connection.request(Op::List).unwrap()["fixture"], 12);
            }
        }
    }

    #[test]
    fn a_protocol_outside_the_offered_range_is_refused() {
        let (client, host) = memory_pair();
        let server = thread::spawn(move || {
            host.read_frame().unwrap().unwrap();
            let chosen = HelloReply {
                protocol: PROTOCOLS.max + 1,
                ..identity()
            };
            write_json(&host, Kind::Json, &HelloAnswer::Accepted(chosen)).unwrap();
        });
        let error = Connection::hello(Arc::new(client), Role::Cli)
            .err()
            .unwrap();
        assert!(
            matches!(&error, ClientError::Protocol(message) if message.contains("does not speak")),
            "{error}"
        );
        server.join().unwrap();
    }
}
