//! The session host's sessions and the requests that act on them (D3, D5, D9,
//! D10, D11, D13).
//!
//! Sessions and panes keep tmux's textual IDs, `$N` and `%N`, from counters that
//! never repeat while the host runs (D9). `create_session` is idempotent by
//! launch ID: a retried request returns the session the first one started,
//! also for a while after that session ended; `launch_status` reports by the
//! same ID how that session ended, with the last lines of its screen, so that
//! a launch can report a program that could not start. Those lines, read once
//! the session's output has drained, also complete the report its supervisor
//! left in the state directory (`state::launches::drained`) before the session
//! leaves the listing. Every request but `attach` and `supervisor_register`,
//! which turn their connection into a stream, is answered here; the
//! connection handling lives in `server.rs`.
//!
//! A session starts only while local sessions are enabled (`enabled`, the
//! marker check of `gate`): a create that races `hgs session-host uninstall`
//! is refused once the marker is gone, and starts nothing.
//!
//! `recovery_worker` starts the supervision of the recovery worker that
//! `server.rs` configures (D14); it ends with the host.
//!
//! The host exits like a tmux server: when its last session has ended, it
//! lingers for `LINGER`, then decides to exit under the registry lock, so a
//! `create_session` that comes later is refused as `busy`, a connection whose
//! hello comes later is not admitted, and either client starts a new host
//! (D3). The connections admitted before the decision are counted, so the host
//! answers what they still send before it exits.
use super::{
    channel::{self, PaneFacts, SupervisorChannel},
    link::Link,
    log,
    protocol::{
        Attach, Attached, CallerSession, Captured, ClientEntry, ClientList, ConsoleEvidence, CreateSession,
        Created, Done, EnvValue, ErrorBody, ErrorCode, Grown, HostStatus, InputClaim, InputReady,
        LaunchState, Listed, Op, PaneDetails, PaneEntry, Resolved, SessionEntry, Signal,
        SupervisorOp, SupervisorRegistration, Target, RUN_MARKER,
    },
    session::{self, lock, Attachment, LaunchPlan, Launcher, Session},
    worker::Worker,
};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock, PoisonError},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// How long an idle host waits for a new session before it exits.
pub(crate) const LINGER: Duration = Duration::from_secs(5);
/// WhiteWh fork: how long a resident host (`hgs session-host start`) lingers
/// without sessions: in effect until `stop`. It is set in the environment the
/// starting client hands the host.
pub(crate) const RESIDENT: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);
pub(crate) const RESIDENT_ENV: &str = "HGS_HOST_RESIDENT";
/// How long the host waits for a supervisor's answer.
const INPUT_STATE_TIMEOUT: Duration = Duration::from_secs(3);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a request waits for a supervisor whose registration was just
/// answered to open its channel.
const SUPERVISOR_OPENING: Duration = Duration::from_secs(2);
/// D11: a terminate waits this long, from its request, for the supervisor to
/// record its exit.
const TERMINATE_GRACE: Duration = Duration::from_secs(10);
/// How long a supervisor's registration waits for its session: the root
/// process runs before its create has registered the session.
const REGISTRATION_WAIT: Duration = Duration::from_secs(10);
/// How long a pause whose supervisor left without an answer waits for the
/// supervisor's end.
const GONE_WAIT: Duration = Duration::from_secs(2);
/// Ended launches that a retried create still finds.
const ENDED_LAUNCHES: usize = 256;
/// What `launch_status` keeps of an ended session's screen: its last lines.
const ENDING_LINES: usize = 8;
const ENDING_BYTES: usize = 2048;
/// The tallest pane `grow_unviewed` makes: its screen model stays small.
const MAX_GROWN_ROWS: u16 = 1024;

/// When an idle host exits (D3).
#[derive(Debug)]
pub(crate) struct Linger {
    period: Duration,
    idle_since: Option<Instant>,
}

/// What the host does next.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Next {
    /// Sessions run: wait for a change.
    Serve,
    /// Idle: exit unless something changes within this time.
    Linger(Duration),
    Exit,
}

impl Linger {
    pub(crate) fn new(period: Duration) -> Self {
        Self {
            period,
            idle_since: None,
        }
    }

    pub(crate) fn next(&mut self, idle: bool, now: Instant) -> Next {
        if !idle {
            self.idle_since = None;
            return Next::Serve;
        }
        let since = *self.idle_since.get_or_insert(now);
        match self
            .period
            .checked_sub(now.saturating_duration_since(since))
        {
            Some(left) if !left.is_zero() => Next::Linger(left),
            _ => Next::Exit,
        }
    }
}

enum Launch {
    Running(u64),
    /// What the request that started it was told, with `existing` set, and
    /// how the session ended.
    Ended(Created, LaunchState),
}

/// The end of `text`, a screen: its last `lines` lines that are not blank,
/// trailing spaces trimmed, cut to at most `bytes` bytes from its end.
pub(crate) fn last_lines(text: &str, lines: usize, bytes: usize) -> String {
    let kept: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect();
    let tail = kept[kept.len().saturating_sub(lines)..].join("\n");
    let mut start = tail.len().saturating_sub(bytes);
    while !tail.is_char_boundary(start) {
        start += 1;
    }
    tail[start..].to_owned()
}

struct Registry {
    sessions: BTreeMap<u64, Arc<Session>>,
    launches: HashMap<String, Launch>,
    /// Ended launches, oldest first.
    ended: VecDeque<String>,
    next_session: u64,
    next_pane: u64,
    next_client: u64,
    /// Creates between their checks and their sessions' registration.
    starting: usize,
    /// The name of the session being created (creates run one at a time), so
    /// that no rename takes it meanwhile.
    naming: Option<String>,
    /// Requests being answered.
    requests: usize,
    /// Connections whose hello was accepted and that have not ended.
    connections: usize,
    /// `stop_host` was accepted.
    stop: bool,
    /// The host has decided to exit: no session starts any more.
    exiting: bool,
    supervisors: HashMap<u64, Arc<SupervisorChannel>>,
}

/// Starts the supervision of the recovery worker (D14).
type StartWorker = Box<dyn Fn() -> Result<Worker, String> + Send + Sync>;

/// The recovery worker that the host supervises once a client asks (D14).
#[derive(Default)]
struct Recovery {
    /// None where the host supervises no worker.
    start: Option<StartWorker>,
    running: Option<Worker>,
    /// The host is exiting: no worker starts any more.
    stopped: bool,
}

/// The session host's state, shared by every connection.
pub(crate) struct Host<L: Launcher> {
    launcher: L,
    /// The host's own process ID (`pane_info`'s `server_pid`).
    pid: u32,
    /// `bundled` or `inbox` (D6).
    conpty: String,
    /// Reads another process's command line for input checks; None when it
    /// cannot.
    argv: fn(u32) -> Option<Vec<String>>,
    /// Whether local sessions are enabled now (D2): checked before each new
    /// session starts.
    enabled: fn() -> bool,
    registry: Mutex<Registry>,
    changed: Condvar,
    /// Task #14 (P2.4): raised when a session starts, ends, is renamed or a
    /// client attaches or leaves; `list` reports it, so a reader sees the structure move without
    /// comparing every entry.
    structure: std::sync::atomic::AtomicU64,
    /// Creates run one at a time, so names and launch IDs are checked once.
    creating: Mutex<()>,
    recovery: Mutex<Recovery>,
    /// Where the launches of the host's state directory keep their evidence;
    /// unset, the host completes no launch report.
    launch_reports: OnceLock<PathBuf>,
}

fn error(code: ErrorCode, message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(code, message)
}

fn payload(value: &impl Serialize) -> Result<Value, ErrorBody> {
    serde_json::to_value(value).map_err(|e| error(ErrorCode::Internal, e.to_string()))
}

/// A session name tmux would take: not empty, no control characters.
fn check_name(name: &str) -> Result<(), ErrorBody> {
    if name.is_empty() || name.chars().any(char::is_control) {
        return Err(error(
            ErrorCode::Invalid,
            format!("invalid session name {name:?}"),
        ));
    }
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}

/// The number in a `c<N>` client ID.
fn client_number(id: &str) -> Option<u64> {
    id.strip_prefix('c')?.parse().ok()
}

fn client_id(number: u64) -> String {
    format!("c{number}")
}

/// The session `target` names in `registry`.
fn lookup(registry: &Registry, target: &Target) -> Option<Arc<Session>> {
    let mut sessions = registry.sessions.values();
    match target {
        Target::Name(name) => sessions.find(|session| session.name() == *name),
        Target::Session(id) => sessions.find(|session| session.session_id() == *id),
        Target::Pane(id) => sessions.find(|session| session.pane_id() == *id),
    }
    .cloned()
}

/// tmux's message for a target that names nothing.
fn missing(target: &Target) -> ErrorBody {
    let message = match target {
        Target::Name(name) | Target::Session(name) => format!("can't find session: {name}"),
        Target::Pane(pane) => format!("can't find pane: {pane}"),
    };
    error(ErrorCode::NotFound, message)
}

impl<L: Launcher> Host<L> {
    pub(crate) fn new(
        launcher: L,
        pid: u32,
        conpty: &str,
        argv: fn(u32) -> Option<Vec<String>>,
        enabled: fn() -> bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            launcher,
            pid,
            conpty: conpty.into(),
            argv,
            enabled,
            registry: Mutex::new(Registry {
                sessions: BTreeMap::new(),
                launches: HashMap::new(),
                ended: VecDeque::new(),
                next_session: 0,
                next_pane: 0,
                next_client: 0,
                starting: 0,
                naming: None,
                requests: 0,
                connections: 0,
                stop: false,
                exiting: false,
                supervisors: HashMap::new(),
            }),
            changed: Condvar::new(),
            structure: std::sync::atomic::AtomicU64::new(0),
            creating: Mutex::new(()),
            recovery: Mutex::default(),
            launch_reports: OnceLock::new(),
        })
    }

    /// Complete the launch reports in `folder` as their sessions end.
    pub(crate) fn complete_launch_reports_in(&self, folder: PathBuf) {
        let _ = self.launch_reports.set(folder);
    }

    /// Supervise the recovery worker with `start` once a client asks (D14).
    pub(crate) fn supervise_recovery_with(
        &self,
        start: impl Fn() -> Result<Worker, String> + Send + Sync + 'static,
    ) {
        lock(&self.recovery).start = Some(Box::new(start));
    }

    /// Stop the supervised workers for good; the host is exiting.
    pub(crate) fn stop_workers(&self) {
        let running = {
            let mut recovery = lock(&self.recovery);
            recovery.stopped = true;
            recovery.running.take()
        };
        if let Some(worker) = running {
            worker.stop();
        }
    }

    /// D14: supervise the recovery worker from now on, unless the host does.
    fn recovery_worker(&self) -> Result<(), ErrorBody> {
        let mut recovery = lock(&self.recovery);
        if recovery.stopped {
            return Err(error(
                ErrorCode::Busy,
                "the session host is exiting; start it again",
            ));
        }
        if recovery.running.is_some() {
            return Ok(());
        }
        let Some(start) = &recovery.start else {
            return Err(error(
                ErrorCode::Unsupported,
                "this session host supervises no recovery worker",
            ));
        };
        let worker = start().map_err(|message| {
            error(
                ErrorCode::Internal,
                format!("cannot supervise the recovery worker: {message}"),
            )
        })?;
        recovery.running = Some(worker);
        Ok(())
    }

    fn registry(&self) -> std::sync::MutexGuard<'_, Registry> {
        lock(&self.registry)
    }

    fn changed(&self) {
        self.changed.notify_all();
    }

    /// Task #14: `changed`, when a session started, ended, was renamed or
    /// gained or lost a client: `list`'s structure counter moves.
    fn restructured(&self) {
        self.structure.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.changed();
    }

    /// The session `target` names.
    pub(crate) fn find(&self, target: &Target) -> Result<Arc<Session>, ErrorBody> {
        lookup(&self.registry(), target).ok_or_else(|| missing(target))
    }

    /// `find`, waiting at most `timeout` while sessions are being created.
    fn find_created(&self, target: &Target, timeout: Duration) -> Result<Arc<Session>, ErrorBody> {
        let deadline = Instant::now() + timeout;
        let mut registry = self.registry();
        loop {
            if let Some(session) = lookup(&registry, target) {
                return Ok(session);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if registry.starting == 0 || left.is_zero() {
                return Err(missing(target));
            }
            registry = self
                .changed
                .wait_timeout(registry, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Answer one request. `attach` and `supervisor_register` belong to
    /// `server.rs`, which turns their connections into streams.
    pub(crate) fn request(self: &Arc<Self>, op: Op) -> Result<Value, ErrorBody> {
        self.registry().requests += 1;
        let result = self.answer(op);
        self.registry().requests -= 1;
        self.changed();
        result
    }

    fn answer(self: &Arc<Self>, op: Op) -> Result<Value, ErrorBody> {
        match op {
            Op::CreateSession(request) => payload(&self.create(request)?),
            Op::LaunchStatus { launch_id } => payload(&self.launch_status(&launch_id)?),
            Op::List => payload(&self.list()),
            Op::ResolveTarget { target } => {
                let session = self.find(&target)?;
                payload(&Resolved {
                    session: session.session_id(),
                    name: session.name(),
                    pane: session.pane_id(),
                })
            }
            Op::PaneInfo { target } => payload(&self.pane_info(&*self.find(&target)?)),
            Op::Capture { target, styled } => payload(&Captured {
                text: self.find(&target)?.capture(styled),
            }),
            Op::GrowUnviewed { target, rows } => {
                if rows > MAX_GROWN_ROWS {
                    return Err(error(
                        ErrorCode::Invalid,
                        format!("a pane grows to at most {MAX_GROWN_ROWS} rows"),
                    ));
                }
                payload(&Grown {
                    grown: self.find(&target)?.grow_unviewed(rows)?,
                })
            }
            Op::SendKeys { target, keys } => {
                let session = self.find(&target)?;
                session.send_keys(&keys, || self.evidence(&session).ok())?;
                payload(&Done {})
            }
            Op::Paste {
                target,
                text,
                bracketed,
            } => {
                self.find(&target)?.paste(&text, bracketed)?;
                payload(&Done {})
            }
            Op::SetEnvironment {
                target,
                name,
                value,
            } => {
                if name.is_empty() || name.contains('\0') {
                    return Err(error(
                        ErrorCode::Invalid,
                        format!("invalid variable name {name:?}"),
                    ));
                }
                self.find(&target)?.set_variable(&name, value.as_deref());
                payload(&Done {})
            }
            Op::ShowEnvironment { target, name } => payload(&EnvValue {
                value: self.find(&target)?.variable(&name),
            }),
            Op::Rename { target, name } => {
                self.rename(&target, &name)?;
                payload(&Done {})
            }
            Op::Kill { target } => {
                self.find(&target)?.kill();
                payload(&Done {})
            }
            Op::Signal { target, signal } => {
                self.signal(&target, signal)?;
                payload(&Done {})
            }
            Op::InputState(claim) => payload(&self.input_state(&claim)?),
            Op::ListClients { target } => {
                let sessions = match target {
                    Some(target) => vec![self.find(&target)?],
                    None => self.registry().sessions.values().cloned().collect(),
                };
                payload(&ClientList {
                    clients: sessions.iter().flat_map(|s| clients_of(s)).collect(),
                })
            }
            Op::Notify { client, text } => {
                let number = client_number(&client);
                let delivered = number.is_some_and(|number| {
                    let sessions: Vec<_> = self.registry().sessions.values().cloned().collect();
                    sessions.iter().any(|session| session.notify(number, &text))
                });
                if !delivered {
                    return Err(error(
                        ErrorCode::NotFound,
                        format!("can't find client: {client}"),
                    ));
                }
                payload(&Done {})
            }
            Op::HostStatus => {
                let registry = self.registry();
                payload(&HostStatus {
                    sessions: registry.sessions.len() as u32,
                    clients: registry
                        .sessions
                        .values()
                        .map(|session| session.clients().len() as u32)
                        .sum(),
                    conpty: self.conpty.clone(),
                })
            }
            Op::StopHost => {
                let mut registry = self.registry();
                if !registry.sessions.is_empty() || registry.starting > 0 {
                    return Err(error(
                        ErrorCode::Busy,
                        "sessions are running; the session host keeps running",
                    ));
                }
                registry.stop = true;
                payload(&Done {})
            }
            Op::RecoveryWorker => {
                self.recovery_worker()?;
                payload(&Done {})
            }
            Op::Attach(_) | Op::SupervisorRegister(_) => Err(error(
                ErrorCode::Invalid,
                "this request changes the connection and needs one of its own",
            )),
            // Task #33: only the connection knows its client's process.
            Op::CallerSession => Err(error(
                ErrorCode::Invalid,
                "caller_session is answered for a connection's own client",
            )),
            Op::Unsupported => Err(error(
                ErrorCode::Unsupported,
                "this session host does not know the request",
            )),
        }
    }

    /// Start a session, or report the one this launch ID started.
    pub(crate) fn create(self: &Arc<Self>, request: CreateSession) -> Result<Created, ErrorBody> {
        let _one_at_a_time = lock(&self.creating);
        let invalid = |message: &str| Err(error(ErrorCode::Invalid, message));
        if request.launch_id.is_empty() {
            return invalid("a session needs a launch ID");
        }
        check_name(&request.name)?;
        let Some(program) = request.argv.first() else {
            return invalid("a session needs a command");
        };
        if !Path::new(program).is_absolute() {
            return invalid("the session's program must be an absolute path");
        }
        if !Path::new(&request.cwd).is_absolute() {
            return invalid("the session's directory must be an absolute path");
        }
        if request.size.cols == 0 || request.size.rows == 0 {
            return invalid("a session needs at least one column and one row");
        }
        {
            let registry = self.registry();
            match registry.launches.get(&request.launch_id) {
                Some(Launch::Running(id)) => {
                    let session = &registry.sessions[id];
                    return Ok(Created {
                        session: session.session_id(),
                        pane: session.pane_id(),
                        name: session.name(),
                        pid: session.pid,
                        existing: true,
                    });
                }
                Some(Launch::Ended(created, _)) => return Ok(created.clone()),
                None => {}
            }
        }
        // Outside the registry lock, as it reads a file. Creates run one at a
        // time, so no other create adds this launch ID meanwhile.
        if !(self.enabled)() {
            return Err(error(
                ErrorCode::Refused,
                "local sessions are not enabled on this computer; run hgs session-host install",
            ));
        }
        let (id, pane) = {
            let mut registry = self.registry();
            if registry.exiting {
                return Err(error(
                    ErrorCode::Busy,
                    "the session host is exiting; start it again",
                ));
            }
            if registry
                .sessions
                .values()
                .any(|session| session.name() == request.name)
            {
                return Err(error(
                    ErrorCode::Exists,
                    format!("duplicate session: {}", request.name),
                ));
            }
            let ids = (registry.next_session, registry.next_pane);
            registry.next_session += 1;
            registry.next_pane += 1;
            registry.starting += 1;
            registry.naming = Some(request.name.clone());
            ids
        };
        let started = self.launch(&request, id, pane);
        let mut registry = self.registry();
        registry.starting -= 1;
        registry.naming = None;
        let session = match started {
            Ok(session) => session,
            Err(error) => {
                drop(registry);
                self.changed();
                return Err(error);
            }
        };
        registry
            .launches
            .insert(request.launch_id.clone(), Launch::Running(id));
        registry.sessions.insert(id, session.clone());
        drop(registry);
        // Watched only once registered: a root process that exits at once
        // must find its session there when it ends.
        if let Err(error) = self.watch(&session) {
            let mut registry = self.registry();
            registry.sessions.remove(&id);
            registry.launches.remove(&request.launch_id);
            drop(registry);
            session.kill();
            session.finish(None);
            self.restructured();
            return Err(error);
        }
        self.restructured();
        Ok(Created {
            session: session.session_id(),
            pane: session.pane_id(),
            name: request.name.clone(),
            pid: session.pid,
            existing: false,
        })
    }

    /// How the session `launch_id` started is doing.
    fn launch_status(&self, launch_id: &str) -> Result<LaunchState, ErrorBody> {
        let registry = self.registry();
        match registry.launches.get(launch_id) {
            Some(Launch::Running(id)) => {
                let session = &registry.sessions[id];
                Ok(LaunchState {
                    session: session.session_id(),
                    pane: session.pane_id(),
                    name: session.name(),
                    dead: session.dead(),
                    ended: false,
                    code: None,
                    output: String::new(),
                })
            }
            Some(Launch::Ended(_, state)) => Ok(state.clone()),
            None => Err(error(
                ErrorCode::NotFound,
                format!("can't find launch: {launch_id}"),
            )),
        }
    }

    fn launch(
        self: &Arc<Self>,
        request: &CreateSession,
        id: u64,
        pane: u64,
    ) -> Result<Arc<Session>, ErrorBody> {
        let failed = |message: String| error(ErrorCode::Internal, message);
        let environment =
            session::environment(&request.env, &format!("${id}"), &format!("%{pane}"))
                .map_err(|e| error(ErrorCode::Invalid, e.to_string()))?;
        let launched = self
            .launcher
            .launch(&LaunchPlan {
                argv: &request.argv,
                cwd: &request.cwd,
                environment: &environment,
                size: request.size,
            })
            .map_err(|e| failed(format!("cannot start the session: {e}")))?;
        Session::start(id, pane, request, unix_now(), launched)
            .map_err(|e| failed(format!("cannot start the session: {e}")))
    }

    /// Wait for a registered session's end on a thread of its own.
    fn watch(self: &Arc<Self>, session: &Arc<Session>) -> Result<(), ErrorBody> {
        let host = self.clone();
        let watched = session.clone();
        thread::Builder::new()
            .name(format!("hgs-pane-{}-wait", session.pane))
            .spawn(move || {
                let code = watched.watch();
                host.ended(&watched, code);
            })
            .map(drop)
            .map_err(|e| {
                error(
                    ErrorCode::Internal,
                    format!("cannot watch the session: {e}"),
                )
            })
    }

    /// A session's root process exited and its console and job are gone.
    fn ended(&self, session: &Arc<Session>, code: Option<i32>) {
        // How the supervisor ended: 128 and above for a signal, a crash or a
        // pause it reported (D9, D11).
        log::note(format!(
            "session {} ended: {}",
            session.session_id(),
            code.map_or_else(
                || "exit code unknown".into(),
                |code| format!("exit {code} ({:#010x})", code as u32)
            )
        ));
        // The output has drained: the screen holds what the program said
        // last, such as why it could not start.
        let output = last_lines(&session.joined_text(), ENDING_LINES, ENDING_BYTES);
        // Before a listing can miss the session: what its supervisor read of
        // the screen may lack the last lines.
        if let Some(folder) = self.launch_reports.get() {
            crate::state::launches::drained(folder, &session.launch_id, &output);
        }
        let mut registry = self.registry();
        registry.sessions.remove(&session.id);
        registry.launches.insert(
            session.launch_id.clone(),
            Launch::Ended(
                Created {
                    session: session.session_id(),
                    pane: session.pane_id(),
                    name: session.name(),
                    pid: session.pid,
                    existing: true,
                },
                LaunchState {
                    session: session.session_id(),
                    pane: session.pane_id(),
                    name: session.name(),
                    dead: true,
                    ended: true,
                    code,
                    output,
                },
            ),
        );
        registry.ended.push_back(session.launch_id.clone());
        while registry.ended.len() > ENDED_LAUNCHES {
            if let Some(old) = registry.ended.pop_front() {
                if matches!(registry.launches.get(&old), Some(Launch::Ended(..))) {
                    registry.launches.remove(&old);
                }
            }
        }
        let channel = registry.supervisors.remove(&session.id);
        drop(registry);
        if let Some(channel) = channel {
            channel.close();
        }
        session.finish(code);
        self.restructured();
    }

    fn list(&self) -> Listed {
        let sessions: Vec<_> = self.registry().sessions.values().cloned().collect();
        let mut listed = Listed {
            structure: Some(self.structure.load(std::sync::atomic::Ordering::SeqCst)),
            ..Listed::default()
        };
        for session in &sessions {
            let name = session.name();
            let clients = clients_of(session);
            listed.sessions.push(SessionEntry {
                id: session.session_id(),
                name: name.clone(),
                attached: clients.len() as u64,
                created: session.created,
                launch_id: session.launch_id.clone(),
            });
            listed.panes.push(PaneEntry {
                session: name,
                session_id: session.session_id(),
                pane: session.pane_id(),
                pid: session.pid,
                dead: session.dead(),
                run_marker: session.variable(RUN_MARKER).unwrap_or_default(),
                // Where the session started: another process's current
                // directory lives in its memory, which the host does not read
                // (platform::current_directories), so a program that changes
                // directory is still listed where it started.
                current_path: session.cwd.clone(),
                output: Some(session.output_count()),
            });
            listed.clients.extend(clients);
        }
        listed
    }

    fn pane_info(&self, session: &Session) -> PaneDetails {
        let info = session.info();
        let cells = |value: usize| u16::try_from(value).unwrap_or(u16::MAX);
        PaneDetails {
            session_id: session.session_id(),
            session_name: session.name(),
            pane: session.pane_id(),
            server_pid: self.pid,
            pane_pid: session.pid,
            width: cells(info.width),
            height: cells(info.height),
            cursor_x: cells(info.cursor_x),
            cursor_y: cells(info.cursor_y),
            in_mode: false,
            current_command: session.current_command(),
            dead: session.dead(),
            run_marker: session.variable(RUN_MARKER).unwrap_or_default(),
            bracketed_paste: info.bracketed_paste,
            tty: format!("conpty:{}", session.pane_id()),
        }
    }

    fn rename(&self, target: &Target, name: &str) -> Result<(), ErrorBody> {
        check_name(name)?;
        let session = self.find(target)?;
        let registry = self.registry();
        if registry.naming.as_deref() == Some(name)
            || registry
                .sessions
                .values()
                .any(|other| other.id != session.id && other.name() == name)
        {
            return Err(error(
                ErrorCode::Exists,
                format!("duplicate session: {name}"),
            ));
        }
        session.set_name(name);
        drop(registry);
        self.restructured();
        Ok(())
    }

    /// The session's open supervisor channel. One whose registration answer
    /// is still on its way opens within moments: it is waited for.
    fn supervisor(&self, session: &Session) -> Result<Arc<SupervisorChannel>, ErrorBody> {
        let channel = self.registry().supervisors.get(&session.id).cloned();
        channel
            .filter(|channel| channel.wait_open(SUPERVISOR_OPENING))
            .ok_or_else(|| {
                error(
                    ErrorCode::Refused,
                    format!(
                        "session {} has no supervisor connected to the session host",
                        session.name()
                    ),
                )
            })
    }

    /// D11 through the supervisor channel.
    fn signal(&self, target: &Target, signal: Signal) -> Result<(), ErrorBody> {
        let session = self.find(target)?;
        let channel = self.supervisor(&session)?;
        match signal {
            Signal::Pause => match channel.call(SupervisorOp::Stop, STOP_TIMEOUT) {
                Ok(_) => Ok(()),
                // The supervisor raised Ctrl+Break and ended with its agent
                // before its answer arrived: the pause did what it asks.
                Err(_) if channel.is_closed() && session.wait_exit(GONE_WAIT) => Ok(()),
                Err(error) => Err(error),
            },
            Signal::Terminate => {
                let deadline = Instant::now() + TERMINATE_GRACE;
                let terminate = move || {
                    let _ = channel.call(SupervisorOp::Stop, STOP_TIMEOUT);
                    if !session.wait_exit(deadline.saturating_duration_since(Instant::now())) {
                        session.kill();
                    }
                };
                thread::Builder::new()
                    .name("hgs-terminate".into())
                    .spawn(terminate)
                    .map(drop)
                    .map_err(|e| error(ErrorCode::Internal, e.to_string()))
            }
        }
    }

    /// The console evidence of `session`'s supervisor (D10).
    fn evidence(&self, session: &Session) -> Result<ConsoleEvidence, ErrorBody> {
        let channel = self.supervisor(session)?;
        serde_json::from_value(channel.call(SupervisorOp::InputState, INPUT_STATE_TIMEOUT)?)
            .map_err(|e| {
                error(
                    ErrorCode::Internal,
                    format!("invalid console evidence from the supervisor: {e}"),
                )
            })
    }

    fn input_state(&self, claim: &InputClaim) -> Result<InputReady, ErrorBody> {
        let refused = |message: &str| error(ErrorCode::Refused, message);
        let session = self.find(&claim.target)?;
        let run_marker = session.variable(RUN_MARKER).unwrap_or_default();
        let name = session.name();
        channel::check_pane(
            claim,
            &PaneFacts {
                name: &name,
                run_marker: &run_marker,
                dead: session.dead(),
                bracketed_paste: session.info().bracketed_paste,
            },
        )
        .map_err(refused)?;
        let evidence = self.evidence(&session)?;
        channel::judge(claim, session.pid, &evidence, &self.argv).map_err(refused)?;
        let info = session.info();
        let cells = |value: usize| u16::try_from(value).unwrap_or(u16::MAX);
        Ok(InputReady {
            cursor_x: cells(info.cursor_x),
            cursor_y: cells(info.cursor_y),
        })
    }

    /// Attach a client running as `client_pid` (D13). The connection then
    /// carries `Attachment`'s output and the client's input.
    pub(crate) fn attach(
        &self,
        request: &Attach,
        client_pid: u32,
    ) -> Result<(Arc<Session>, Arc<Attachment>, Attached), ErrorBody> {
        if request.size.cols == 0 || request.size.rows == 0 {
            return Err(error(
                ErrorCode::Invalid,
                "a client needs at least one column and one row",
            ));
        }
        let session = self.find(&request.target)?;
        if let Some(run_id) = &request.run_id {
            if session.variable(RUN_MARKER).as_ref() != Some(run_id) {
                return Err(error(
                    ErrorCode::Refused,
                    format!("session {} runs another run now", session.name()),
                ));
            }
        }
        let number = {
            let mut registry = self.registry();
            registry.next_client += 1;
            registry.next_client
        };
        let client = Attachment::new(number, client_pid, request.size);
        session.attach(client.clone())?;
        let attached = Attached {
            client: client_id(number),
            session: session.session_id(),
            name: session.name(),
            pane: session.pane_id(),
        };
        self.restructured();
        Ok((session, client, attached))
    }

    /// A client left its session.
    pub(crate) fn detached(&self, session: &Session, client: &Attachment) {
        session.detach(client.id);
        self.restructured();
    }

    /// Task #33: the session whose root process or job holds `client_pid`,
    /// the connection's own kernel-verified client. Session jobs never share a
    /// process and forbid breakaway, so at most one session holds it.
    pub(crate) fn caller_session(&self, client_pid: u32) -> Result<Value, ErrorBody> {
        let sessions: Vec<Arc<Session>> = self.registry().sessions.values().cloned().collect();
        let found = sessions.iter().find(|session| session.holds(client_pid)).ok_or_else(|| {
            error(ErrorCode::NotFound, "this process runs in no session of this host")
        })?;
        payload(&CallerSession {
            session: found.session_id(),
            name: found.name(),
            pane: found.pane_id(),
            run_marker: found.variable(RUN_MARKER).unwrap_or_default(),
        })
    }

    /// Accept `link` as the supervisor channel of a session (D10), from a
    /// client running as `client_pid`: the session's root process or one of
    /// its descendants in the session's job. A supervisor starts before its
    /// create has registered the session, so a session being created is
    /// waited for.
    pub(crate) fn register_supervisor(
        &self,
        registration: &SupervisorRegistration,
        client_pid: u32,
        link: Arc<dyn Link>,
    ) -> Result<Arc<SupervisorChannel>, ErrorBody> {
        let session = self.find_created(
            &Target::Session(registration.session.clone()),
            REGISTRATION_WAIT,
        )?;
        if session.pane_id() != registration.pane {
            return Err(error(
                ErrorCode::NotFound,
                format!("can't find pane: {}", registration.pane),
            ));
        }
        if registration.pid != client_pid || !session.holds(client_pid) {
            return Err(error(
                ErrorCode::Refused,
                "only a process of the session may supervise it",
            ));
        }
        let mut registry = self.registry();
        // A session that ended meanwhile keeps no channel.
        if !registry
            .sessions
            .get(&session.id)
            .is_some_and(|registered| Arc::ptr_eq(registered, &session))
        {
            return Err(error(
                ErrorCode::NotFound,
                format!("can't find session: {}", registration.session),
            ));
        }
        if registry
            .supervisors
            .get(&session.id)
            .is_some_and(|channel| !channel.is_closed())
        {
            return Err(error(
                ErrorCode::Exists,
                format!("session {} has a supervisor already", session.name()),
            ));
        }
        let channel = SupervisorChannel::new(link);
        registry.supervisors.insert(session.id, channel.clone());
        Ok(channel)
    }

    /// Block until the host should exit: no session has run for `linger`, or
    /// `stop_host` was accepted while none ran. From then on no session starts.
    pub(crate) fn wait_for_exit(&self, linger: Duration) {
        let mut linger = Linger::new(linger);
        let mut registry = self.registry();
        loop {
            let idle = registry.sessions.is_empty() && registry.starting == 0;
            if std::mem::take(&mut registry.stop) && idle {
                registry.exiting = true;
                return;
            }
            registry = match linger.next(idle, Instant::now()) {
                Next::Exit => {
                    registry.exiting = true;
                    return;
                }
                Next::Linger(left) => {
                    self.changed
                        .wait_timeout(registry, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                Next::Serve => self
                    .changed
                    .wait(registry)
                    .unwrap_or_else(PoisonError::into_inner),
            };
        }
    }

    /// Count a connection whose hello is about to be accepted until the
    /// returned admission is dropped. None once the host has decided to exit:
    /// that connection then ends before its hello is answered, and its client
    /// looks for the next host (D3).
    pub(crate) fn admit(self: &Arc<Self>) -> Option<Admission<L>> {
        let mut registry = self.registry();
        if registry.exiting {
            return None;
        }
        registry.connections += 1;
        Some(Admission(self.clone()))
    }

    /// After the decision to exit: wait at most `timeout` for the requests
    /// being answered and for the connections accepted before the decision,
    /// whose clients may still send a request, to end.
    pub(crate) fn drain(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut registry = self.registry();
        while registry.requests > 0 || registry.connections > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            registry = self
                .changed
                .wait_timeout(registry, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    pub(crate) fn exiting(&self) -> bool {
        self.registry().exiting
    }
}

/// A connection the host accepted (`Host::admit`), counted while it lives.
pub(crate) struct Admission<L: Launcher>(Arc<Host<L>>);

impl<L: Launcher> Drop for Admission<L> {
    fn drop(&mut self) {
        self.0.registry().connections -= 1;
        self.0.changed();
    }
}

fn clients_of(session: &Session) -> Vec<ClientEntry> {
    let name = session.name();
    session
        .clients()
        .into_iter()
        .map(|(number, pid)| ClientEntry {
            id: client_id(number),
            session: name.clone(),
            pid,
        })
        .collect()
}
