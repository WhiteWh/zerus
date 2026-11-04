//! The Windows session host as the terminal backend (spec D3, D5, D8). Every
//! method is one request of protocol 1 (`host::protocol`) on a connection of its
//! own; the host keeps tmux's names, IDs and messages, so callers check what they
//! always have:
//!
//! | backend | host request |
//! |---|---|
//! | `listing`, `attached_counts`, `panes`, `pane_paths` | one `list` |
//! | `session_exists`, `session_name`, `enclosing_session` | `resolve_target` |
//! | `pane_info`, `probe` | `pane_info`, its fields in `PaneField` order, flags as `1` or `0` |
//! | `on_terminal` | `pane_info`, and the agent's parent is the pane's supervisor |
//! | `start`, `launch -d` | `create_session`, starting the host when none runs; `launch -d` then `launch_status` |
//! | `attach`, `launch` | `attach` on a connection of its own (role `attach`); `launch` creates first |
//! | `attachable` | none: this process's console and its attach modes |
//! | `set_run_marker` | `set_environment` of `RUN_MARKER` |
//! | `set_client`, `viewer` | `set_environment`, `show_environment` of the session variable `HGS_CLIENT` (a launch's `HGS_CLIENT` is in the supervisor's environment only) |
//! | `rename` | `rename` |
//! | `kill_session` | `signal` `terminate`; `kill` for a session without a supervisor |
//! | `kill_pane`, `kill` | `kill` |
//! | `request_exit` | `signal` `pause` |
//! | `input_state` | `input_state` |
//! | `capture` | `capture` |
//! | `send`, `forward_key` | `send_keys` |
//! | `stage`, `paste`, `discard` | one `paste`: staged text stays in this process |
//! | `notify` | `notify` |
//! | `server_running` | `host_status` |
//! | `completes_launch_reports` | none: the host completes a launch's report as its session ends |
//!
//! **No host (D3).** Only `start` starts a host. Without one there are no live
//! sessions, as tmux has none without a server; any other failure (a busy or
//! refusing host, another protocol) is an error, never an empty listing, which
//! would show live sessions as stopped and resumable.
//!
//! **Launch (D8).** A session's root process is the stable CLI's `hgs.exe --run`,
//! which runs the program, PowerShell for `sh`, as its child with Ctrl+C turned
//! back on (D11). It starts with this process's whole environment and the
//! launch's variables (`session_environment`). This side sets
//! `HGS_EXECUTABLE` to the stable CLI, the one hgs.exe that sessions, hooks and
//! records name; the host sets only what it alone knows (`HGS_HOST_SESSION`,
//! `HGS_HOST_PANE`) and the terminal type. A launch's `HGS_LAUNCH_ID` is the
//! host's launch ID, so a retried create never starts a second session; other
//! starts get a launch ID that listings leave out, as tmux lists none for them.
//!
//! **A program that cannot start.** The supervisor reports why it could not
//! start its program (a missing or unreadable executable, a refused hook
//! setup) on the pane, which then ends, before any record exists. A launch,
//! attached or not, therefore watches its new session for `STARTUP` through
//! `launch_status`: one that ends in that time with a failure fails the launch
//! (exit 1) with its exit code and the last lines its pane showed, so that the
//! GUI, which reads a detached launch's exit, reports it instead of waiting for
//! a session that never comes. A detached launch returns once the session has
//! run that long. `start`, which resumes, restores and forks call under the
//! state lock that the supervisor needs, does not wait.
//!
//! **Supervisor (D10, D11).** The `--run` supervisor opens its session's
//! supervisor channel (`host::supervisor`), through which the host pauses the
//! agent (`request_exit`, the pause half of `kill_session`) and reads the
//! console evidence that `input_state` judges. A session whose supervisor has
//! no channel refuses both.
//!
//! **Recovery worker (D14).** `supervise_recovery` asks the running host to
//! supervise the recovery worker; without a host it does nothing.
//!
//! **Attach (D13).** `attach` and a launch without `-d` run the attach client
//! (`host::attach`) on this process's console, which both standard input and
//! output must be and which must take the attach modes (otherwise nothing is
//! created or attached, and `attachable` refuses before the CLI resumes a saved
//! session for an attach, as tmux refuses a client without a terminal). A
//! launch creates the session at the console's
//! size, then attaches by its session ID; one that ended before the attach
//! counts as exited. With a run ID (`hgs a --existing --run-id`) the host
//! checks the run marker as it attaches and a changed run is refused with
//! `RUN_CHANGED`. A detach (`Ctrl+B d`, `Ctrl+B Ctrl+D`) and the session's end
//! exit 0; a lost host connection exits 1 and, when no host runs any more,
//! reports `HOST_DIED` as the tmux backend reports a dead server. Other
//! refusals print the host's message and exit 1, as a failed `tmux attach`.
// Only Windows builds use the host backend; its mapping is tested everywhere.
#![cfg_attr(not(windows), allow(dead_code))]
use super::{
    ClientExit, ClientRow, DryRun, InputClaim, InputState, Key, LaunchSpec, Listing, PaneField,
    PaneInfo, PanePath, PaneRow, Paste, SessionRow, StageErrors, Target, TerminalBackend,
    RUN_CHANGED,
};
use crate::{
    cli::{Error, Result as CliResult},
    host::{
        attach::{self, Outcome, Terminal, NOT_A_CONSOLE},
        client::AttachStream,
        protocol::{
            self, Attach, Attached, Captured, CreateSession, Created, Done, EnvValue, ErrorBody,
            ErrorCode, Grown, HostStatus, InputReady, LaunchState, Listed, Op, PaneDetails,
            Resolved, Signal, Size, RUN_MARKER,
        },
        spawn::{self, Environment},
    },
    state::Result,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Mutex, PoisonError},
    thread,
    time::{Duration, Instant},
};

/// What an attach client reports when its host has gone: the tmux backend's
/// message for a dead server, naming the session host.
pub(crate) const HOST_DIED: &str =
    "hgs: the session host died; terminal restored, sessions on this box are gone";
/// The variable that names the viewing peer (tmux keeps it in the session's
/// environment).
const CLIENT: &str = "HGS_CLIENT";
/// A new session's size until a client attaches: tmux's `default-size`.
pub(crate) const SIZE: Size = Size { cols: 80, rows: 24 };
/// Launch IDs of starts without `--launch-id` begin with this, which no UUID
/// does; listings leave them out.
const STARTED: &str = "hgs-start-";
/// A launch whose session ends within this time, with a failure, reports that
/// its program could not start: the "ended immediately" of the tmux backend's
/// launch (`cli::launch`).
pub(crate) const STARTUP: Duration = Duration::from_secs(3);
/// How long a launch waits, after the startup time, for a session whose root
/// process has exited to end: the host lets its output drain for up to 5 s.
const ENDING: Duration = Duration::from_secs(10);
/// How often a launch asks how its new session is doing.
const STARTUP_POLL: Duration = Duration::from_millis(100);
/// Variables that describe the session a command runs in. A launch from inside
/// a session must not hand them to the new one: tmux starts a pane from its
/// server's environment, which never holds them. A launch sets those its session
/// needs again. `CLAUDECODE` is the mark Claude Code sets for the commands it
/// runs; a Claude Code that finds it refuses to start as one nested in another.
const SESSION_CONTEXT: &[&str] = &[
    "CLAUDECODE",
    "TMUX",
    "TMUX_PANE",
    "HGS_HOST_SESSION",
    "HGS_HOST_PANE",
    "HGS_SESSION",
    "HGS_AGENT",
    "HGS_RUN_ID",
    "HGS_EXPECTED_ID",
    "HGS_REQUESTED_ID",
    "HGS_ARCHIVE_ID",
    "HGS_FRESH",
    "HGS_FRESH_REPLACES",
    "HGS_FORK_PARENT_ID",
    "HGS_FORK_SOURCE_NAME",
    "HGS_FORK_AGENT_HINT_REVISION",
    "HGS_LAUNCH_ID",
    "HGS_ACCOUNT_ID",
    "HGS_TRACKING",
    "HGS_CLIENT",
];

/// Only a complete normalized pair permits dropping inherited raw controls
/// before environment validation. Other values keep the existing refusal path.
fn resolved_hint_snapshot(variables: &[String]) -> bool {
    let value = |name: &str| {
        variables
            .iter()
            .rev()
            .filter_map(|entry| entry.split_once('='))
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    };
    matches!(value("HGS_AGENT_HINT"), Some("0" | "1"))
        && value("HGS_AGENT_HINT_OFF").is_some_and(|off| off.len() <= 8192 && !off.contains('\0'))
}

/// Why a request got no answer.
#[derive(Debug)]
pub(crate) enum Failure {
    /// No host runs (D3): the only failure that means "no sessions".
    NoHost,
    /// The host answered with an error.
    Host(ErrorBody),
    /// The host could not be reached, started or understood.
    Unreachable(String),
}

impl Failure {
    fn message(self) -> String {
        match self {
            Self::NoHost => "no session host runs".into(),
            Self::Host(error) => error.message,
            Self::Unreachable(message) => message,
        }
    }

    /// The host answered that the target does not exist.
    fn not_found(&self) -> bool {
        matches!(self, Self::Host(error) if error.code == ErrorCode::NotFound)
    }
}

/// How the backend reaches the host and what it reads of this process.
pub(crate) trait Connector: Sync {
    /// Send one request to the running host; never starts one.
    fn call(&self, op: Op) -> std::result::Result<Value, Failure>;
    /// Send a `create_session`, starting the host when none runs (D3).
    fn create(&self, op: Op) -> std::result::Result<Value, Failure>;
    /// Attach to a session on a connection of its own, which then carries
    /// the stream (D13); never starts a host.
    fn attach(&self, request: Attach) -> std::result::Result<(Attached, AttachStream), Failure>;
    /// This process's console for an attach client; None when standard input
    /// or output is not one.
    fn terminal(&self) -> Option<Box<dyn Terminal>>;
    /// This process's environment block, which a new session starts from (D8).
    fn environment(&self) -> Result<Environment>;
    /// A session may replace optional hint controls before validating the raw
    /// block. Existing connectors retain their normal strict capture by default.
    fn environment_for_session(&self, _variables: &[String]) -> Result<Environment> {
        self.environment()
    }
    /// The stable CLI (D2, D8).
    fn stable_cli(&self) -> Result<String>;
    /// The verified parent of process `pid`, when known.
    fn parent(&self, pid: u32) -> Option<u32>;
    /// A variable of this process's environment, None when unset or empty.
    fn var(&self, name: &str) -> Option<String>;
}

/// What a launch makes of its new session's state (`startup`).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Startup {
    /// Ask again.
    Wait,
    /// The program runs, or ended at once with exit 0.
    Started,
    /// The program could not start: the launch fails with this message.
    Failed(String),
}

/// What a launch makes of `state`, its session's, `elapsed` after the
/// session was created. A session that ended with a failure (or an unknown
/// exit) before it ran for `window` failed to start, with what its pane
/// showed last; one whose root process exited waits for its end, at most
/// `ENDING` more. An exit code beyond 255 is an NTSTATUS, shown in hex as
/// Windows tools show it.
pub(crate) fn startup(state: &LaunchState, elapsed: Duration, window: Duration) -> Startup {
    if state.ended {
        let code = match state.code {
            Some(0) => return Startup::Started,
            Some(code @ 0..=255) => format!("exit {code}"),
            Some(code) => format!("exit {:#010x}", code as u32),
            None => "exit code unknown".to_owned(),
        };
        let mut message = format!("session {} ended immediately ({code})", state.name);
        if !state.output.is_empty() {
            message.push_str(":\n");
            message.push_str(&state.output);
        }
        return Startup::Failed(message);
    }
    if elapsed >= window && (!state.dead || elapsed >= window + ENDING) {
        Startup::Started
    } else {
        Startup::Wait
    }
}

/// The session host backend over `C`.
pub(crate) struct HostBackend<C> {
    connector: C,
    /// Text `stage` holds for `paste`, by buffer name.
    staged: Mutex<BTreeMap<String, String>>,
    /// `STARTUP`; tests shorten it.
    startup: Duration,
}

impl<C: Connector> HostBackend<C> {
    pub(crate) const fn new(connector: C) -> Self {
        Self {
            connector,
            staged: Mutex::new(BTreeMap::new()),
            startup: STARTUP,
        }
    }

    /// This backend with a launch's startup time `startup` instead of `STARTUP`.
    #[cfg(test)]
    pub(crate) fn with_startup(mut self, startup: Duration) -> Self {
        self.startup = startup;
        self
    }

    fn staged(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, String>> {
        self.staged.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn call<T: DeserializeOwned>(&self, op: Op) -> std::result::Result<T, Failure> {
        let value = self.connector.call(op)?;
        serde_json::from_value(value)
            .map_err(|error| Failure::Unreachable(format!("invalid session host answer: {error}")))
    }

    fn act(&self, op: Op) -> Result<()> {
        self.call::<Done>(op).map(drop).map_err(Failure::message)
    }

    /// The host's `list`; empty without a host.
    fn list(&self) -> Result<Listed> {
        match self.call(Op::List) {
            Err(Failure::NoHost) => Ok(Listed::default()),
            result => result.map_err(Failure::message),
        }
    }

    fn details(&self, target: Target) -> std::result::Result<PaneDetails, Failure> {
        self.call(Op::PaneInfo {
            target: target_of(target),
        })
    }

    /// Start the session `spec` describes at `size`; the host's answer and
    /// the launch ID.
    fn create(&self, spec: &LaunchSpec, size: Size) -> Result<(Created, String)> {
        let environment = session_environment(
            self.connector.environment_for_session(&spec.env)?,
            &spec.env,
            &self.connector.stable_cli()?,
        )?;
        let request = create_request(spec, environment, size, || uuid::Uuid::new_v4().to_string());
        let launch_id = request.launch_id.clone();
        let created = self
            .connector
            .create(Op::CreateSession(request))
            .map_err(Failure::message)?;
        serde_json::from_value::<Created>(created)
            .map(|created| (created, launch_id))
            .map_err(|error| format!("invalid session host answer: {error}"))
    }

    /// Watch the session that launch `launch_id` created at `since` until it
    /// has run for the startup time or ended (`startup`). A host that cannot
    /// tell, such as one of an earlier version, leaves the launch started, as
    /// before.
    fn await_start(&self, launch_id: &str, since: Instant) -> CliResult<()> {
        loop {
            let Ok(state) = self.call::<LaunchState>(Op::LaunchStatus {
                launch_id: launch_id.into(),
            }) else {
                return Ok(());
            };
            match startup(&state, since.elapsed(), self.startup) {
                Startup::Wait => thread::sleep(STARTUP_POLL),
                Startup::Started => return Ok(()),
                Startup::Failed(message) => return Err(Error::new(1, message)),
            }
        }
    }

    /// This process's console for an attach client (D13), refused when it is
    /// not a console or cannot take the attach modes, as a check before
    /// anything starts: the modes themselves wait for the attach, so that
    /// Ctrl+C still interrupts a slow start.
    fn console(&self) -> CliResult<Box<dyn Terminal>> {
        let terminal = self
            .connector
            .terminal()
            .ok_or_else(|| Error::new(1, NOT_A_CONSOLE))?;
        terminal
            .enter()
            .map_err(|error| Error::new(1, error.to_string()))?;
        terminal.leave();
        Ok(terminal)
    }

    /// The attach client of `hgs a` and of a launch without `-d` (D13), on
    /// `terminal`, until a detach, the session's end or the host's loss.
    /// `created` says that this process has just created the session, so
    /// that a session gone before the attach has exited.
    fn interactive(
        &self,
        terminal: &dyn Terminal,
        target: protocol::Target,
        run_id: Option<&str>,
        created: bool,
    ) -> CliResult<ClientExit> {
        let exit = |code, server_died| ClientExit { code, server_died };
        let size = terminal.size().unwrap_or(SIZE);
        // The attach modes first: a console that cannot take them refuses
        // before the pane takes this client's size.
        terminal
            .enter()
            .map_err(|error| Error::new(1, error.to_string()))?;
        let attached = self.connector.attach(Attach {
            target,
            size,
            run_id: run_id.map(str::to_owned),
        });
        let (attached, stream) = match attached {
            Ok(attached) => attached,
            Err(failure) => {
                terminal.leave();
                return match failure {
                    Failure::Host(error)
                        if run_id.is_some() && error.code == ErrorCode::Refused =>
                    {
                        Err(Error::new(1, RUN_CHANGED))
                    }
                    // tmux attaches a new session's client as it creates
                    // it, and reports a program that ended at once so.
                    Failure::Host(error) if created && error.code == ErrorCode::NotFound => {
                        println!("[exited]");
                        Ok(exit(0, false))
                    }
                    failure => {
                        eprintln!("{}", failure.message());
                        Ok(exit(1, false))
                    }
                };
            }
        };
        let outcome = attach::relay(&stream, terminal, &attached.name, size);
        drop(stream);
        terminal.leave();
        match outcome {
            Outcome::Detached | Outcome::Exited(_) => Ok(exit(0, false)),
            Outcome::Failed(error) => Err(Error::new(1, format!("the console failed: {error}"))),
            Outcome::Lost(reason) => {
                if matches!(self.connector.call(Op::HostStatus), Err(Failure::NoHost)) {
                    eprintln!("{HOST_DIED}");
                    Ok(exit(1, true))
                } else {
                    // Whole, as `main` prints errors: the client exits next.
                    let message = format!(
                        "hgs: lost the connection to the session host ({reason}); \
                         the session continues, attach again"
                    );
                    eprintln!("{message}");
                    Ok(exit(1, false))
                }
            }
        }
    }

    /// D14: ask the running host to supervise the recovery worker. Nothing
    /// happens without a host, as no session runs then, nor with a host that
    /// supervises none (one of an older version): a later request reaches the
    /// host that replaces it.
    pub(crate) fn supervise_recovery(&self) -> Result<()> {
        match self.call::<Done>(Op::RecoveryWorker) {
            Ok(_) | Err(Failure::NoHost) => Ok(()),
            Err(Failure::Host(error)) if error.code == ErrorCode::Unsupported => Ok(()),
            Err(failure) => Err(failure.message()),
        }
    }

    fn resolve(&self, target: protocol::Target) -> std::result::Result<Option<Resolved>, Failure> {
        match self.call(Op::ResolveTarget { target }) {
            Ok(resolved) => Ok(Some(resolved)),
            Err(Failure::NoHost) => Ok(None),
            Err(failure) if failure.not_found() => Ok(None),
            Err(failure) => Err(failure),
        }
    }
}

fn target_of(target: Target) -> protocol::Target {
    match target {
        Target::Name(name) => protocol::Target::Name(name.into()),
        Target::Session(id) => protocol::Target::Session(id.into()),
        Target::Pane(id) => protocol::Target::Pane(id.into()),
    }
}

fn flag(value: bool) -> &'static str {
    if value {
        "1"
    } else {
        "0"
    }
}

/// `fields` of `details` as `tmux display-message -p` prints them: tab
/// separated in request order, with a final line feed.
pub(crate) fn render(details: &PaneDetails, fields: &[PaneField]) -> String {
    let mut text = fields
        .iter()
        .map(|field| match field {
            PaneField::SessionId => details.session_id.clone(),
            PaneField::SessionName => details.session_name.clone(),
            PaneField::SocketPath
            | PaneField::SessionCreated
            | PaneField::SessionWindows
            | PaneField::SessionGrouped
            | PaneField::WindowPanes
            | PaneField::WindowLinked => String::new(),
            PaneField::PaneId => details.pane.clone(),
            PaneField::ServerPid => details.server_pid.to_string(),
            PaneField::PanePid => details.pane_pid.to_string(),
            PaneField::Width => details.width.to_string(),
            PaneField::Height => details.height.to_string(),
            PaneField::CursorX => details.cursor_x.to_string(),
            PaneField::CursorY => details.cursor_y.to_string(),
            PaneField::InMode => flag(details.in_mode).into(),
            PaneField::CurrentCommand => details.current_command.clone(),
            PaneField::Dead => flag(details.dead).into(),
            PaneField::RunMarker => details.run_marker.clone(),
            PaneField::BracketedPaste => flag(details.bracketed_paste).into(),
            PaneField::Tty => details.tty.clone(),
        })
        .collect::<Vec<_>>()
        .join("\t");
    text.push('\n');
    text
}

/// The launch ID a listing reports: one that `--launch-id` gave (a UUID), else
/// none.
fn listed_launch_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .ok()
        .filter(|uuid| uuid.to_string() == id)
        .map(|_| id.to_owned())
        .unwrap_or_default()
}

/// The `ls --json` view of a `list`.
pub(crate) fn listing(listed: Listed) -> Listing {
    Listing {
        clients: listed
            .clients
            .into_iter()
            .map(|client| ClientRow {
                session: client.session,
                terminal: client.id,
            })
            .collect(),
        sessions: listed
            .sessions
            .into_iter()
            .map(|session| SessionRow {
                launch_id: listed_launch_id(&session.launch_id),
                name: session.name,
                id: session.id,
                attached: session.attached,
                created: session.created,
            })
            .collect(),
    }
}

/// The environment of a new session's supervisor (D8): `client`, this process's
/// block, without the variables of a session it runs in (`SESSION_CONTEXT`),
/// then the launch's `KEY=VALUE` variables, where an empty value removes the
/// variable (as `nonempty_env` and agents read an empty one), and
/// `HGS_EXECUTABLE`, the stable CLI. Messages name variables, never values.
pub(crate) fn session_environment(
    mut client: Environment,
    variables: &[String],
    executable: &str,
) -> Result<Vec<String>> {
    for name in SESSION_CONTEXT {
        client.remove(name).map_err(|e| e.to_string())?;
    }
    for (index, entry) in variables.iter().enumerate() {
        let (name, value) = entry
            .split_once('=')
            .filter(|(name, _)| !name.is_empty())
            .ok_or_else(|| format!("session variable {index} is not NAME=VALUE"))?;
        if value.is_empty() {
            client.remove(name)
        } else {
            client.set(name, value)
        }
        .map_err(|e| e.to_string())?;
    }
    client
        .set("HGS_EXECUTABLE", executable)
        .map_err(|e| e.to_string())?;
    Ok(client.entries())
}

/// The `create_session` request for `spec` with `environment`, the session's
/// whole block, at `size`. `HGS_LAUNCH_ID` among the launch's variables is the
/// launch ID; otherwise `fresh` gives one.
pub(crate) fn create_request(
    spec: &LaunchSpec,
    environment: Vec<String>,
    size: Size,
    fresh: impl FnOnce() -> String,
) -> CreateSession {
    let launch_id = spec
        .env
        .iter()
        .rev()
        .find_map(|entry| entry.strip_prefix("HGS_LAUNCH_ID="))
        .filter(|id| !id.is_empty())
        .map_or_else(|| format!("{STARTED}{}", fresh()), str::to_owned);
    CreateSession {
        launch_id,
        name: spec.name.clone(),
        cwd: spec.cwd.clone(),
        env: environment,
        argv: spec.command.clone(),
        size,
    }
}

/// A word as a Windows command line quotes it.
fn word(text: &str) -> String {
    spawn::quote_argument(text)
}

/// What `--dry-run` prints on Windows: the session host request, in the shape
/// of the tmux command it replaces. Words are quoted as a Windows command line
/// quotes them; a launch's command is the supervisor's command line.
pub(crate) fn describe(operation: &DryRun) -> String {
    match operation {
        DryRun::ListJson | DryRun::List => "session-host list".into(),
        DryRun::Attach { session, client } => format!(
            "session-host set-environment -t {} {CLIENT} {}\nsession-host attach -t {}",
            word(session),
            word(client),
            word(session)
        ),
        DryRun::AttachExisting { session, run_id } => format!(
            "session-host attach existing session {}{}",
            word(session),
            run_id
                .map(|id| format!(" (run {})", word(id)))
                .unwrap_or_default()
        ),
        DryRun::Kill { session } => format!("session-host kill -t {}", word(session)),
        DryRun::Launch(spec) => create_line(spec, spec.detached),
        DryRun::Start(spec) => create_line(spec, true),
    }
}

fn create_line(spec: &LaunchSpec, detached: bool) -> String {
    let mut words = vec!["session-host".to_owned(), "create-session".to_owned()];
    if detached {
        words.push("-d".into());
    }
    words.extend(["-s".into(), word(&spec.name), "-c".into(), word(&spec.cwd)]);
    for value in &spec.env {
        words.extend(["-e".into(), word(&super::masked(value))]);
    }
    let command = spawn::command_line(&spec.command).unwrap_or_else(|_| {
        spec.command
            .iter()
            .map(|argument| word(argument))
            .collect::<Vec<_>>()
            .join(" ")
    });
    format!("{} -- {command}", words.join(" "))
}

impl<C: Connector> TerminalBackend for HostBackend<C> {
    fn server_running(&self) -> bool {
        match self.call::<HostStatus>(Op::HostStatus) {
            Ok(_) => true,
            Err(Failure::NoHost) => false,
            // A host that does not answer still runs.
            Err(_) => true,
        }
    }
    /// D8: `HGS_HOST_PANE` marks a process inside a session, as `TMUX` does.
    fn inside(&self) -> bool {
        self.connector.var("HGS_HOST_PANE").is_some()
    }
    fn enclosing_session(&self) -> String {
        self.connector
            .var("HGS_HOST_PANE")
            .and_then(|pane| self.resolve(protocol::Target::Pane(pane)).ok().flatten())
            .map(|resolved| resolved.name)
            .unwrap_or_default()
    }
    fn own_pane(&self) -> Result<String> {
        self.connector
            .var("HGS_HOST_PANE")
            .ok_or_else(|| "missing HGS_HOST_PANE in tracked agent launch".into())
    }
    /// The pseudoconsole's output reaches the host until the console closes
    /// after the supervisor has exited; the host then completes the report
    /// (`host::registry`).
    fn completes_launch_reports(&self) -> bool {
        true
    }

    fn listing(&self) -> Result<Listing> {
        self.list().map(listing)
    }
    fn attached_counts(&self) -> Result<Option<Vec<(String, String)>>> {
        let listed = self.list()?;
        Ok((!listed.sessions.is_empty()).then(|| {
            listed
                .sessions
                .into_iter()
                .map(|session| (session.name, session.attached.to_string()))
                .collect()
        }))
    }
    fn panes(&self) -> Result<Vec<PaneRow>> {
        Ok(self
            .list()?
            .panes
            .into_iter()
            .map(|pane| PaneRow {
                session: pane.session,
                session_id: pane.session_id,
                pane: pane.pane,
                dead: flag(pane.dead).into(),
                run_marker: pane.run_marker,
            })
            .collect())
    }
    /// Each session has its one pane, active in its one window.
    fn pane_paths(&self) -> Result<Vec<PanePath>> {
        Ok(self
            .list()?
            .panes
            .into_iter()
            .map(|pane| PanePath {
                session: pane.session,
                session_id: pane.session_id,
                active_window: true,
                active_pane: true,
                path: pane.current_path,
            })
            .collect())
    }

    fn session_exists(&self, name: &str) -> bool {
        matches!(
            self.resolve(protocol::Target::Name(name.into())),
            Ok(Some(_))
        )
    }
    fn session_name(&self, id: &str) -> Result<Option<String>> {
        self.resolve(protocol::Target::Session(id.into()))
            .map(|resolved| resolved.map(|resolved| resolved.name))
            .map_err(Failure::message)
    }
    fn pane_info(&self, target: Target, fields: &[PaneField]) -> Result<PaneInfo> {
        let details = self.details(target).map_err(Failure::message)?;
        Ok(PaneInfo::new(render(&details, fields)))
    }
    fn probe(&self, target: Target, fields: &[PaneField]) -> Option<PaneInfo> {
        self.pane_info(target, fields).ok()
    }
    fn start(&self, spec: &LaunchSpec) -> Result<()> {
        self.create(spec, SIZE).map(drop)
    }
    fn set_run_marker(&self, name: &str, run_id: &str) -> Result<()> {
        self.act(Op::SetEnvironment {
            target: protocol::Target::Name(name.into()),
            name: RUN_MARKER.into(),
            value: Some(run_id.into()),
        })
    }
    fn rename(&self, id: &str, name: &str) -> Result<()> {
        self.act(Op::Rename {
            target: protocol::Target::Session(id.into()),
            name: name.into(),
        })
    }
    /// D11: the agent is asked to stop and given time to record its exit
    /// before the session ends. A session without a supervisor channel (one
    /// whose supervisor could not register) ends as tmux `kill-session` ends it.
    fn kill_session(&self, target: Target) -> Result<()> {
        let target = target_of(target);
        match self.call::<Done>(Op::Signal {
            target: target.clone(),
            signal: Signal::Terminate,
        }) {
            Ok(_) => Ok(()),
            Err(Failure::Host(error)) if error.code == ErrorCode::Refused => {
                self.act(Op::Kill { target })
            }
            Err(failure) => Err(failure.message()),
        }
    }
    fn kill_pane(&self, pane: &str) -> Result<()> {
        self.act(Op::Kill {
            target: protocol::Target::Pane(pane.into()),
        })
    }

    /// D11: the pane's supervisor raises Ctrl+Break for its process group: the
    /// agent and the tools it started, which on Unix end with the agent. The
    /// supervisor stays to record the exit; the session then ends. The caller
    /// has checked `pid` against the record that pins it, and the supervisor
    /// knows its agent.
    fn request_exit(&self, pane: &str, _pid: u32) -> Result<()> {
        self.act(Op::Signal {
            target: protocol::Target::Pane(pane.into()),
            signal: Signal::Pause,
        })
    }
    fn input_state(&self, claim: &InputClaim) -> Result<InputState> {
        let ready: InputReady = self
            .call(Op::InputState(protocol::InputClaim {
                target: protocol::Target::Pane(claim.pane.into()),
                name: claim.name.into(),
                run_id: claim.run_id.into(),
                agent: claim.agent.into(),
                pid: claim.pid.and_then(|pid| u32::try_from(pid).ok()),
            }))
            .map_err(Failure::message)?;
        Ok(InputState {
            cursor_x: ready.cursor_x.to_string(),
            cursor_y: ready.cursor_y.to_string(),
        })
    }
    /// A pane's `Tty` is `conpty:%N`. A tracked agent is on it when the pane's
    /// supervisor started it, which the system's verified parent link shows;
    /// until the supervisor channel reports the console's processes (D10).
    fn on_terminal(&self, pid: &str, tty: &str) -> Result<bool> {
        let (Some(pane), Ok(pid)) = (tty.strip_prefix("conpty:"), pid.parse::<u32>()) else {
            return Ok(false);
        };
        let details = self.details(Target::Pane(pane)).map_err(Failure::message)?;
        Ok(details.tty == tty
            && details.pane_pid != 0
            && self.connector.parent(pid) == Some(details.pane_pid))
    }
    /// A host older than `grow_unviewed` answers `unsupported`: its panes keep
    /// their size, so a clipped panel stays in Terminal.
    fn grow_unviewed(&self, pane: &str, rows: u16) -> Result<bool> {
        match self.call::<Grown>(Op::GrowUnviewed {
            target: protocol::Target::Pane(pane.into()),
            rows,
        }) {
            Ok(grown) => Ok(grown.grown),
            Err(Failure::Host(error)) if error.code == ErrorCode::Unsupported => Ok(false),
            Err(failure) => Err(failure.message()),
        }
    }
    fn capture(&self, pane: &str, mode: super::Capture) -> Result<Vec<u8>> {
        let captured: Captured = self
            .call(Op::Capture {
                target: protocol::Target::Pane(pane.into()),
                styled: matches!(mode, super::Capture::Styled),
            })
            .map_err(Failure::message)?;
        Ok(captured.text.into_bytes())
    }
    fn send(&self, pane: &str, keys: &[Key]) -> Result<()> {
        self.act(Op::SendKeys {
            target: protocol::Target::Pane(pane.into()),
            keys: keys
                .iter()
                .map(|key| match key {
                    Key::Named(name) => protocol::Key::Named((*name).into()),
                    Key::Literal(text) => protocol::Key::Literal((*text).into()),
                })
                .collect(),
        })
    }
    /// The text waits in this process for `paste`, which sends it in one
    /// request: nothing reaches another process's argv. Text that is not UTF-8
    /// is not taken.
    fn stage(&self, buffer: &str, text: &[u8], _errors: StageErrors) -> Result<bool> {
        let Ok(text) = String::from_utf8(text.to_vec()) else {
            return Ok(false);
        };
        self.staged().insert(buffer.into(), text);
        Ok(true)
    }
    fn paste(&self, buffer: &str, pane: &str, mode: Paste) -> Result<()> {
        let text = self
            .staged()
            .remove(buffer)
            .ok_or_else(|| format!("no buffer {buffer}"))?;
        let bracketed = match mode {
            Paste::Composer => true,
            Paste::Terminal { bracketed } => bracketed,
        };
        self.act(Op::Paste {
            target: protocol::Target::Pane(pane.into()),
            text,
            bracketed,
        })
    }
    fn discard(&self, buffer: &str) {
        self.staged().remove(buffer);
    }

    /// D13: an attached launch needs this process's console before anything
    /// starts, and the session starts at its size. A session that ends within
    /// `STARTUP` with a failure fails the launch with what its pane showed
    /// last: a program that cannot start is reported, attached or not (a
    /// detached launch waits that long for it). The state lock is not held
    /// here: the supervisor takes it before it starts the program.
    fn launch(&self, spec: &LaunchSpec) -> CliResult<ClientExit> {
        if spec.detached {
            let (created, launch_id) = self
                .create(spec, SIZE)
                .map_err(|error| Error::new(1, error))?;
            if !created.existing {
                self.await_start(&launch_id, Instant::now())?;
            }
            return Ok(ClientExit {
                code: 0,
                server_died: false,
            });
        }
        let terminal = self.console()?;
        let size = terminal.size().unwrap_or(SIZE);
        let (created, launch_id) = self
            .create(spec, size)
            .map_err(|error| Error::new(1, error))?;
        let since = Instant::now();
        let exit = self.interactive(
            &*terminal,
            protocol::Target::Session(created.session),
            None,
            true,
        )?;
        // Only a session that has exited is waited for, not one the client
        // left at once.
        if !created.existing
            && exit.code == 0
            && !exit.server_died
            && since.elapsed() < self.startup
            && self
                .call::<LaunchState>(Op::LaunchStatus {
                    launch_id: launch_id.clone(),
                })
                .is_ok_and(|state| state.dead)
        {
            self.await_start(&launch_id, since)?;
        }
        Ok(exit)
    }
    fn attachable(&self) -> CliResult<()> {
        self.console().map(drop)
    }
    fn set_client(&self, target: Target, client: &str) {
        let _ = self.act(Op::SetEnvironment {
            target: target_of(target),
            name: CLIENT.into(),
            value: Some(client.into()),
        });
    }
    fn attach(&self, target: Target, run_id: Option<&str>) -> CliResult<ClientExit> {
        let terminal = self
            .connector
            .terminal()
            .ok_or_else(|| Error::new(1, NOT_A_CONSOLE))?;
        self.interactive(&*terminal, target_of(target), run_id, false)
    }
    /// The host's message goes to the user's terminal, and the exit code is
    /// 1, as tmux reports a failed `kill-session`.
    fn kill(&self, name: &str) -> CliResult<i32> {
        match self.act(Op::Kill {
            target: protocol::Target::Name(name.into()),
        }) {
            Ok(()) => Ok(0),
            Err(message) => {
                eprintln!("{message}");
                Ok(1)
            }
        }
    }
    fn viewer(&self, name: &str) -> String {
        self.call::<EnvValue>(Op::ShowEnvironment {
            target: protocol::Target::Name(name.into()),
            name: CLIENT.into(),
        })
        .ok()
        .and_then(|value| value.value)
        .unwrap_or_default()
    }
    fn notify(&self, client: &str, text: &str) {
        let _ = self.act(Op::Notify {
            client: client.into(),
            text: text.into(),
        });
    }
    fn forward_key(&self, pane: &str, key: &str) {
        let _ = self.send(pane, &[Key::Named(key)]);
    }
    fn describe(&self, operation: &DryRun) -> String {
        describe(operation)
    }
}

#[cfg(windows)]
pub(crate) use self::windows::HOST;

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::host::{
        client::{self, ClientError},
        gate,
        protocol::Role,
        server,
    };
    use std::{
        thread,
        time::{Duration, Instant},
    };

    /// How long a create keeps trying while a host is exiting (D3).
    const EXITING_HOST: Duration = Duration::from_secs(20);

    /// The backend of a computer whose local sessions are enabled.
    pub(crate) static HOST: HostBackend<Pipe> = HostBackend::new(Pipe);

    /// The host of this user's state directory, over its named pipe.
    pub(crate) struct Pipe;

    impl From<ClientError> for Failure {
        fn from(error: ClientError) -> Self {
            match error {
                ClientError::NoHost => Self::NoHost,
                error => Self::Unreachable(error.to_string()),
            }
        }
    }

    impl Connector for Pipe {
        fn call(&self, op: Op) -> std::result::Result<Value, Failure> {
            // Without the state directory no host runs.
            let state = server::existing_state_dir()
                .map_err(|error| Failure::Unreachable(error.to_string()))?
                .ok_or(Failure::NoHost)?;
            let mut connection = client::connect(&state, Role::Cli)?;
            connection.request(op).map_err(Failure::Host)
        }

        /// A host that has decided to exit refuses the create as `busy`; the
        /// create then goes to the host that replaces it.
        fn create(&self, op: Op) -> std::result::Result<Value, Failure> {
            let state =
                server::state_dir().map_err(|error| Failure::Unreachable(error.to_string()))?;
            let deadline = Instant::now() + EXITING_HOST;
            loop {
                let mut connection = client::connect_or_start(&state, Role::Cli, &|| {
                    gate::host_image(&state).map_err(ClientError::Start)
                })?;
                match connection.request(op.clone()) {
                    Err(error) if error.code == ErrorCode::Busy && Instant::now() < deadline => {
                        drop(connection);
                        thread::sleep(Duration::from_millis(200));
                    }
                    result => return result.map_err(Failure::Host),
                }
            }
        }

        fn attach(
            &self,
            request: Attach,
        ) -> std::result::Result<(Attached, AttachStream), Failure> {
            let state = server::existing_state_dir()
                .map_err(|error| Failure::Unreachable(error.to_string()))?
                .ok_or(Failure::NoHost)?;
            client::connect(&state, Role::Attach)?
                .attach(request)
                .map_err(Failure::Host)
        }

        fn terminal(&self) -> Option<Box<dyn Terminal>> {
            crate::host::attach::Console::open()
                .map(|console| Box::new(console) as Box<dyn Terminal>)
        }

        fn environment(&self) -> Result<Environment> {
            Environment::current().map_err(|e| e.to_string())
        }

        fn environment_for_session(&self, variables: &[String]) -> Result<Environment> {
            if resolved_hint_snapshot(variables) {
                Environment::current_for_hint_snapshot().map_err(|e| e.to_string())
            } else {
                self.environment()
            }
        }

        fn stable_cli(&self) -> Result<String> {
            crate::platform::stable_executable().map(|path| path.display().to_string())
        }

        fn parent(&self, pid: u32) -> Option<u32> {
            crate::platform::process_identity(pid)
                .map(|identity| identity.parent)
                .filter(|parent| *parent != 0)
        }

        fn var(&self, name: &str) -> Option<String> {
            crate::state::nonempty_env(name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::protocol::{ClientEntry, PaneEntry, SessionEntry};
    use serde_json::json;

    type Answer = Box<dyn Fn(&Op) -> std::result::Result<Value, Failure> + Sync>;

    /// A host that answers with `answer` and records every request.
    struct Fake {
        answer: Answer,
        requests: Mutex<Vec<(bool, Op)>>,
        variables: BTreeMap<&'static str, String>,
        parents: BTreeMap<u32, u32>,
    }

    impl Fake {
        fn new(
            answer: impl Fn(&Op) -> std::result::Result<Value, Failure> + Sync + 'static,
        ) -> Self {
            Self {
                answer: Box::new(answer),
                requests: Mutex::new(Vec::new()),
                variables: BTreeMap::new(),
                parents: BTreeMap::new(),
            }
        }

        fn done() -> Self {
            Self::new(|_| Ok(json!({})))
        }
    }

    impl Connector for Fake {
        fn call(&self, op: Op) -> std::result::Result<Value, Failure> {
            let answer = (self.answer)(&op);
            self.requests.lock().unwrap().push((false, op));
            answer
        }
        fn create(&self, op: Op) -> std::result::Result<Value, Failure> {
            let answer = (self.answer)(&op);
            self.requests.lock().unwrap().push((true, op));
            answer
        }
        fn attach(
            &self,
            request: Attach,
        ) -> std::result::Result<(Attached, AttachStream), Failure> {
            self.requests
                .lock()
                .unwrap()
                .push((false, Op::Attach(request)));
            Err(Failure::Unreachable(
                "this fake host takes no clients".into(),
            ))
        }
        /// Standard input and output are pipes, as in a test.
        fn terminal(&self) -> Option<Box<dyn Terminal>> {
            None
        }
        fn environment(&self) -> Result<Environment> {
            Environment::parse([
                r"Path=C:\Windows",
                "=C:=C:\\work",
                "TMUX=/tmp/tmux-1000/default,1,0",
                "HGS_SESSION=claude/outer/main",
                "HGS_RUN_ID=outer-run",
                "HGS_HOST_PANE=%9",
                "HGS_STATE_DIR=C:\\state",
                "ANTHROPIC_API_KEY=secret",
                "CLAUDE_CONFIG_DIR=C:\\claude",
            ])
            .map_err(|e| e.to_string())
        }
        fn stable_cli(&self) -> Result<String> {
            Ok(r"C:\Users\u\.local\bin\hgs.exe".into())
        }
        fn parent(&self, pid: u32) -> Option<u32> {
            self.parents.get(&pid).copied()
        }
        fn var(&self, name: &str) -> Option<String> {
            self.variables.get(name).cloned()
        }
    }

    fn requests(backend: &HostBackend<Fake>) -> Vec<(bool, Op)> {
        std::mem::take(&mut *backend.connector.requests.lock().unwrap())
    }

    fn calls(backend: &HostBackend<Fake>) -> Vec<Op> {
        requests(backend).into_iter().map(|(_, op)| op).collect()
    }

    fn host_error(code: ErrorCode, message: &str) -> Failure {
        Failure::Host(ErrorBody::new(code, message))
    }

    fn details() -> PaneDetails {
        PaneDetails {
            session_id: "$3".into(),
            session_name: "claude/p/t".into(),
            pane: "%4".into(),
            server_pid: 70,
            pane_pid: 71,
            width: 120,
            height: 40,
            cursor_x: 2,
            cursor_y: 39,
            in_mode: false,
            current_command: "node".into(),
            dead: false,
            run_marker: "run-1".into(),
            bracketed_paste: true,
            tty: "conpty:%4".into(),
        }
    }

    fn listed() -> Listed {
        Listed {
            sessions: vec![
                SessionEntry {
                    id: "$1".into(),
                    name: "claude/p/a".into(),
                    attached: 1,
                    created: 1_700_000_000,
                    launch_id: "0b6f4c9e-3c1d-4b8f-9a35-6f2e2d6f6a10".into(),
                },
                SessionEntry {
                    id: "$2".into(),
                    name: "fake_agent/p/b".into(),
                    attached: 0,
                    created: 1_700_000_001,
                    launch_id: "hgs-start-0b6f4c9e-3c1d-4b8f-9a35-6f2e2d6f6a11".into(),
                },
            ],
            panes: vec![
                PaneEntry {
                    session: "claude/p/a".into(),
                    session_id: "$1".into(),
                    pane: "%1".into(),
                    pid: 11,
                    dead: false,
                    run_marker: "run-a".into(),
                    current_path: String::new(),
                },
                PaneEntry {
                    session: "fake_agent/p/b".into(),
                    session_id: "$2".into(),
                    pane: "%2".into(),
                    pid: 12,
                    dead: true,
                    run_marker: String::new(),
                    current_path: r"C:\work\p".into(),
                },
            ],
            clients: vec![ClientEntry {
                id: "c1".into(),
                session: "claude/p/a".into(),
                pid: 99,
            }],
        }
    }

    #[test]
    fn pane_facts_render_in_request_order_with_flags_as_digits() {
        use PaneField::*;
        let all = [
            SessionId,
            SessionName,
            PaneId,
            ServerPid,
            PanePid,
            Width,
            Height,
            CursorX,
            CursorY,
            InMode,
            CurrentCommand,
            Dead,
            RunMarker,
            BracketedPaste,
            Tty,
        ];
        assert_eq!(
            render(&details(), &all),
            "$3\tclaude/p/t\t%4\t70\t71\t120\t40\t2\t39\t0\tnode\t0\trun-1\t1\tconpty:%4\n"
        );
        let info = PaneInfo::new(render(&details(), &[SessionId]));
        assert_eq!(info.value(), "$3");
        let info = PaneInfo::new(render(&details(), &[CursorX, CursorY]));
        assert_eq!(info.pair(), Some(("2", "39")));
        let dead = PaneDetails {
            dead: true,
            in_mode: true,
            bracketed_paste: false,
            ..details()
        };
        assert_eq!(
            PaneInfo::new(render(&dead, &[Dead, InMode, BracketedPaste])).fields(),
            ["1", "1", "0"]
        );

        let backend = HostBackend::new(Fake::new(|_| Ok(serde_json::to_value(details()).unwrap())));
        let info = backend
            .pane_info(Target::Name("claude/p/t"), &[RunMarker, SessionId])
            .unwrap();
        assert_eq!(info.fields(), ["run-1", "$3"]);
        assert_eq!(
            calls(&backend),
            [Op::PaneInfo {
                target: protocol::Target::Name("claude/p/t".into())
            }]
        );
        let missing = HostBackend::new(Fake::new(|_| {
            Err(host_error(ErrorCode::NotFound, "can't find pane: %9"))
        }));
        assert_eq!(
            missing.pane_info(Target::Pane("%9"), &[Dead]).err(),
            Some("can't find pane: %9".into())
        );
        assert!(missing.probe(Target::Pane("%9"), &[Dead]).is_none());
    }

    #[test]
    fn one_list_answers_every_listing_and_only_no_host_is_empty() {
        let backend = HostBackend::new(Fake::new(|_| Ok(serde_json::to_value(listed()).unwrap())));
        let listing = backend.listing().unwrap();
        let sessions: Vec<_> = listing
            .sessions
            .iter()
            .map(|row| {
                (
                    row.name.as_str(),
                    row.attached,
                    row.created,
                    row.launch_id.as_str(),
                )
            })
            .collect();
        // A launch ID that `--launch-id` gave is listed; one hgs made up is not.
        assert_eq!(
            sessions,
            [
                (
                    "claude/p/a",
                    1,
                    1_700_000_000,
                    "0b6f4c9e-3c1d-4b8f-9a35-6f2e2d6f6a10"
                ),
                ("fake_agent/p/b", 0, 1_700_000_001, ""),
            ]
        );
        let clients: Vec<_> = listing
            .clients
            .iter()
            .map(|client| (client.session.as_str(), client.terminal.as_str()))
            .collect();
        assert_eq!(clients, [("claude/p/a", "c1")]);
        // Sessions and panes carry the immutable session IDs that a rename keeps.
        let ids: Vec<_> = listing.sessions.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["$1", "$2"]);
        let pane_ids: Vec<_> = backend
            .panes()
            .unwrap()
            .into_iter()
            .map(|row| row.session_id)
            .collect();
        assert_eq!(pane_ids, ["$1", "$2"]);
        let panes: Vec<_> = backend
            .panes()
            .unwrap()
            .into_iter()
            .map(|row| (row.session, row.pane, row.dead, row.run_marker))
            .collect();
        assert_eq!(
            panes,
            [
                ("claude/p/a".into(), "%1".into(), "0".into(), "run-a".into()),
                (
                    "fake_agent/p/b".into(),
                    "%2".into(),
                    "1".into(),
                    String::new()
                ),
            ]
        );
        let paths: Vec<_> = backend
            .pane_paths()
            .unwrap()
            .into_iter()
            .map(|path| {
                (
                    path.session,
                    path.session_id,
                    path.active_window,
                    path.active_pane,
                    path.path,
                )
            })
            .collect();
        assert_eq!(
            paths[1],
            (
                "fake_agent/p/b".into(),
                "$2".into(),
                true,
                true,
                r"C:\work\p".into()
            )
        );
        assert_eq!(
            backend.attached_counts().unwrap(),
            Some(vec![
                ("claude/p/a".into(), "1".into()),
                ("fake_agent/p/b".into(), "0".into())
            ])
        );
        assert!(calls(&backend).iter().all(|op| *op == Op::List));

        let none = HostBackend::new(Fake::new(|_| Err(Failure::NoHost)));
        let listing = none.listing().unwrap();
        assert!(listing.sessions.is_empty() && listing.clients.is_empty());
        assert!(none.panes().unwrap().is_empty() && none.pane_paths().unwrap().is_empty());
        assert_eq!(none.attached_counts().unwrap(), None);
        assert!(!none.server_running());
        // Never started by a listing.
        assert!(requests(&none).iter().all(|(starting, _)| !starting));

        for failure in [
            || {
                host_error(
                    ErrorCode::Busy,
                    "the session host is exiting; start it again",
                )
            },
            || Failure::Unreachable("all pipe instances are busy".into()),
        ] {
            let broken = HostBackend::new(Fake::new(move |_| Err(failure())));
            assert!(broken.listing().is_err());
            assert!(broken.panes().is_err());
            assert!(broken.pane_paths().is_err());
            assert!(broken.attached_counts().is_err());
            assert!(broken.server_running());
        }
    }

    /// A listed `current_path` (the directory a host session started in) is
    /// the session's live folder, which `ls --json` reports as its `cwd`; ""
    /// gives none.
    #[test]
    fn listed_current_paths_are_the_live_session_folders() {
        #[cfg(windows)]
        const WORK: &str = r"C:\work\p";
        #[cfg(not(windows))]
        const WORK: &str = "/work/p";
        let mut listed = listed();
        listed.panes[1].current_path = WORK.into();
        let wire = serde_json::to_value(listed).unwrap();
        let backend: &'static HostBackend<Fake> =
            Box::leak(Box::new(HostBackend::new(Fake::new(move |_| {
                Ok(wire.clone())
            }))));
        let cwds = super::super::with_fake(backend, crate::state::workspace::pane_cwds);
        assert_eq!(
            cwds,
            BTreeMap::from([("fake_agent/p/b".to_string(), WORK.to_string())])
        );
        assert!(calls(backend).iter().all(|op| *op == Op::List));
    }

    #[test]
    fn targets_resolve_and_missing_ones_are_none() {
        let backend = HostBackend::new(Fake::new(|op| match op {
            Op::ResolveTarget {
                target: protocol::Target::Name(name) | protocol::Target::Session(name),
            } if name == "claude/p/a" || name == "$1" => Ok(json!({
                "session": "$1", "name": "claude/p/a", "pane": "%1"
            })),
            Op::ResolveTarget {
                target: protocol::Target::Pane(pane),
            } if pane == "%1" => Ok(json!({"session": "$1", "name": "claude/p/a", "pane": "%1"})),
            Op::ResolveTarget { .. } => Err(host_error(ErrorCode::NotFound, "can't find session")),
            _ => Err(Failure::NoHost),
        }));
        assert!(backend.session_exists("claude/p/a"));
        assert!(!backend.session_exists("claude/p/b"));
        assert_eq!(
            backend.session_name("$1").unwrap(),
            Some("claude/p/a".into())
        );
        assert_eq!(backend.session_name("$7").unwrap(), None);
        assert_eq!(
            calls(&backend)[..2],
            [
                Op::ResolveTarget {
                    target: protocol::Target::Name("claude/p/a".into())
                },
                Op::ResolveTarget {
                    target: protocol::Target::Name("claude/p/b".into())
                },
            ]
        );
        let none = HostBackend::new(Fake::new(|_| Err(Failure::NoHost)));
        assert!(!none.session_exists("claude/p/a"));
        assert_eq!(none.session_name("$1").unwrap(), None);
        let busy = HostBackend::new(Fake::new(|_| Err(host_error(ErrorCode::Busy, "busy"))));
        assert!(!busy.session_exists("claude/p/a"));
        assert_eq!(busy.session_name("$1").unwrap_err(), "busy");

        // Inside a session: HGS_HOST_PANE, as TMUX on Unix.
        assert!(!backend.inside());
        assert_eq!(
            backend.own_pane().unwrap_err(),
            "missing HGS_HOST_PANE in tracked agent launch"
        );
        assert_eq!(backend.enclosing_session(), "");
        let mut inside =
            Fake::new(|_| Ok(json!({"session": "$1", "name": "claude/p/a", "pane": "%1"})));
        inside.variables.insert("HGS_HOST_PANE", "%1".into());
        let inside = HostBackend::new(inside);
        assert!(inside.inside());
        assert_eq!(inside.own_pane().unwrap(), "%1");
        // Its supervisor's launch report waits for the host's drained lines.
        assert!(inside.completes_launch_reports());
        assert_eq!(inside.enclosing_session(), "claude/p/a");
        assert_eq!(
            calls(&inside),
            [Op::ResolveTarget {
                target: protocol::Target::Pane("%1".into())
            }]
        );
    }

    #[test]
    fn actions_send_the_requests_they_stand_for() {
        let backend = HostBackend::new(Fake::new(|op| match op {
            Op::Capture { styled, .. } => {
                Ok(json!({"text": if *styled {"\u{1b}[1mx\u{1b}[0m\n"} else {"x\n"}}))
            }
            Op::ShowEnvironment { .. } => Ok(json!({"value": "laptop"})),
            _ => Ok(json!({})),
        }));
        let pane = || protocol::Target::Pane("%4".into());
        backend.set_run_marker("claude/p/t", "run-2").unwrap();
        backend.rename("$3", "claude/p/u").unwrap();
        backend.kill_pane("%4").unwrap();
        backend
            .send("%4", &[Key::Literal("/clear"), Key::Named("Enter")])
            .unwrap();
        backend.send("%4", &[]).unwrap();
        assert_eq!(
            backend.capture("%4", super::super::Capture::Plain).unwrap(),
            b"x\n"
        );
        assert_eq!(
            backend
                .capture("%4", super::super::Capture::Styled)
                .unwrap(),
            b"\x1b[1mx\x1b[0m\n"
        );
        backend.set_client(Target::Session("$3"), "laptop");
        assert_eq!(backend.viewer("claude/p/t"), "laptop");
        backend.notify("c2", "hgs: copied");
        backend.forward_key("%4", "C-v");
        assert_eq!(backend.kill("claude/p/t").unwrap(), 0);
        assert_eq!(
            calls(&backend),
            [
                Op::SetEnvironment {
                    target: protocol::Target::Name("claude/p/t".into()),
                    name: RUN_MARKER.into(),
                    value: Some("run-2".into())
                },
                Op::Rename {
                    target: protocol::Target::Session("$3".into()),
                    name: "claude/p/u".into()
                },
                Op::Kill { target: pane() },
                Op::SendKeys {
                    target: pane(),
                    keys: vec![
                        protocol::Key::Literal("/clear".into()),
                        protocol::Key::Named("Enter".into())
                    ]
                },
                Op::SendKeys {
                    target: pane(),
                    keys: Vec::new()
                },
                Op::Capture {
                    target: pane(),
                    styled: false
                },
                Op::Capture {
                    target: pane(),
                    styled: true
                },
                Op::SetEnvironment {
                    target: protocol::Target::Session("$3".into()),
                    name: "HGS_CLIENT".into(),
                    value: Some("laptop".into())
                },
                Op::ShowEnvironment {
                    target: protocol::Target::Name("claude/p/t".into()),
                    name: "HGS_CLIENT".into()
                },
                Op::Notify {
                    client: "c2".into(),
                    text: "hgs: copied".into()
                },
                Op::SendKeys {
                    target: pane(),
                    keys: vec![protocol::Key::Named("C-v".into())]
                },
                Op::Kill {
                    target: protocol::Target::Name("claude/p/t".into())
                },
            ]
        );
        assert!(requests(&backend).is_empty());

        let refusing = HostBackend::new(Fake::new(|_| {
            Err(host_error(
                ErrorCode::Exists,
                "duplicate session: claude/p/u",
            ))
        }));
        assert_eq!(
            refusing.rename("$3", "claude/p/u").unwrap_err(),
            "duplicate session: claude/p/u"
        );
        assert_eq!(refusing.kill("claude/p/t").unwrap(), 1);
        assert_eq!(refusing.viewer("claude/p/t"), "");
    }

    #[test]
    fn staged_text_goes_out_in_one_paste() {
        let backend = HostBackend::new(Fake::done());
        let paste = |text: &str, bracketed| Op::Paste {
            target: protocol::Target::Pane("%4".into()),
            text: text.into(),
            bracketed,
        };
        assert!(backend
            .stage(
                "hgs-input-1",
                "line one\nline two".as_bytes(),
                StageErrors::Read
            )
            .unwrap());
        assert!(calls(&backend).is_empty(), "staging stays in this process");
        backend.paste("hgs-input-1", "%4", Paste::Composer).unwrap();
        assert!(backend
            .stage("t-1", b"plain", StageErrors::Discard)
            .unwrap());
        backend
            .paste("t-1", "%4", Paste::Terminal { bracketed: false })
            .unwrap();
        assert!(backend
            .stage("t-2", b"marked", StageErrors::Discard)
            .unwrap());
        backend
            .paste("t-2", "%4", Paste::Terminal { bracketed: true })
            .unwrap();
        assert_eq!(
            calls(&backend),
            [
                paste("line one\nline two", true),
                paste("plain", false),
                paste("marked", true)
            ]
        );
        // A paste releases its text; a discarded one is gone.
        assert_eq!(
            backend.paste("t-2", "%4", Paste::Composer).unwrap_err(),
            "no buffer t-2"
        );
        assert!(backend.stage("t-3", b"never", StageErrors::Read).unwrap());
        backend.discard("t-3");
        backend.discard("t-3");
        assert!(backend.paste("t-3", "%4", Paste::Composer).is_err());
        assert!(!backend
            .stage("t-4", b"\xff\xfe", StageErrors::Read)
            .unwrap());
        assert!(calls(&backend).is_empty());
    }

    #[test]
    fn terminate_asks_the_supervisor_and_kills_a_session_without_one() {
        let target = || protocol::Target::Session("$3".into());
        let backend = HostBackend::new(Fake::done());
        backend.kill_session(Target::Session("$3")).unwrap();
        assert_eq!(
            calls(&backend),
            [Op::Signal {
                target: target(),
                signal: Signal::Terminate
            }]
        );
        let untracked = HostBackend::new(Fake::new(|op| match op {
            Op::Signal { .. } => Err(host_error(
                ErrorCode::Refused,
                "session x has no supervisor connected to the session host",
            )),
            _ => Ok(json!({})),
        }));
        untracked.kill_session(Target::Session("$3")).unwrap();
        assert_eq!(
            calls(&untracked),
            [
                Op::Signal {
                    target: target(),
                    signal: Signal::Terminate
                },
                Op::Kill { target: target() }
            ]
        );
        let gone = HostBackend::new(Fake::new(|_| {
            Err(host_error(ErrorCode::NotFound, "can't find session: $3"))
        }));
        assert_eq!(
            gone.kill_session(Target::Session("$3")).unwrap_err(),
            "can't find session: $3"
        );
        assert_eq!(calls(&gone).len(), 1);
    }

    #[test]
    fn a_pause_is_the_supervisor_pause_of_the_pane() {
        let backend = HostBackend::new(Fake::done());
        backend.request_exit("%4", 72).unwrap();
        assert_eq!(
            calls(&backend),
            [Op::Signal {
                target: protocol::Target::Pane("%4".into()),
                signal: Signal::Pause
            }]
        );
        // Without a supervisor nothing is stopped, and the caller learns why.
        let untracked = HostBackend::new(Fake::new(|_| {
            Err(host_error(
                ErrorCode::Refused,
                "session x has no supervisor connected to the session host",
            ))
        }));
        assert_eq!(
            untracked.request_exit("%4", 72).unwrap_err(),
            "session x has no supervisor connected to the session host"
        );
        assert_eq!(calls(&untracked).len(), 1);
    }

    #[test]
    fn unviewed_panes_grow_through_a_host_that_knows_how() {
        let backend = HostBackend::new(Fake::new(|op| match op {
            Op::GrowUnviewed { rows: 48, .. } => Ok(json!({"grown": true})),
            Op::GrowUnviewed { .. } => Ok(json!({"grown": false})),
            op => panic!("unexpected {op:?}"),
        }));
        assert!(backend.grow_unviewed("%4", 48).unwrap());
        assert!(!backend.grow_unviewed("%4", 96).unwrap());
        assert_eq!(
            requests(&backend)[0],
            (
                false,
                Op::GrowUnviewed {
                    target: protocol::Target::Pane("%4".into()),
                    rows: 48
                }
            )
        );
        // A host of an earlier version keeps the size; other failures stay errors.
        let older = HostBackend::new(Fake::new(|_| {
            Err(host_error(ErrorCode::Unsupported, "unsupported"))
        }));
        assert!(!older.grow_unviewed("%4", 48).unwrap());
        let gone = HostBackend::new(Fake::new(|_| {
            Err(host_error(ErrorCode::NotFound, "pane %4 has exited"))
        }));
        assert_eq!(
            gone.grow_unviewed("%4", 48).unwrap_err(),
            "pane %4 has exited"
        );
    }

    #[test]
    fn the_recovery_worker_is_asked_of_a_running_host_only() {
        let backend = HostBackend::new(Fake::done());
        backend.supervise_recovery().unwrap();
        assert_eq!(requests(&backend), [(false, Op::RecoveryWorker)]);
        // No host, or one that supervises no worker (an older one): nothing to do.
        for quiet in [
            Failure::NoHost,
            host_error(ErrorCode::Unsupported, "unsupported"),
        ] {
            let quiet = std::sync::Mutex::new(Some(quiet));
            let backend = HostBackend::new(Fake::new(move |_| {
                Err(quiet.lock().unwrap().take().unwrap())
            }));
            backend.supervise_recovery().unwrap();
        }
        let failing = HostBackend::new(Fake::new(|_| {
            Err(host_error(
                ErrorCode::Internal,
                "cannot supervise the recovery worker: not installed",
            ))
        }));
        assert_eq!(
            failing.supervise_recovery().unwrap_err(),
            "cannot supervise the recovery worker: not installed"
        );
    }

    #[test]
    fn input_checks_go_to_the_host_and_the_pane_supervisor() {
        let backend = HostBackend::new(Fake::new(|op| match op {
            Op::InputState(_) => Ok(json!({"cursor_x": 3, "cursor_y": 12})),
            _ => Ok(serde_json::to_value(details()).unwrap()),
        }));
        let state = backend
            .input_state(&InputClaim {
                pane: "%4",
                name: "claude/p/t",
                run_id: "run-1",
                agent: "claude",
                pid: Some(72),
            })
            .unwrap();
        assert_eq!(
            (state.cursor_x.as_str(), state.cursor_y.as_str()),
            ("3", "12")
        );
        assert_eq!(
            calls(&backend),
            [Op::InputState(protocol::InputClaim {
                target: protocol::Target::Pane("%4".into()),
                name: "claude/p/t".into(),
                run_id: "run-1".into(),
                agent: "claude".into(),
                pid: Some(72),
            })]
        );
        let refused = HostBackend::new(Fake::new(|_| {
            Err(host_error(
                ErrorCode::Refused,
                "terminal is not in the expected agent input mode; open Terminal",
            ))
        }));
        let claim = InputClaim {
            pane: "%4",
            name: "claude/p/t",
            run_id: "run-1",
            agent: "claude",
            pid: None,
        };
        assert_eq!(
            refused.input_state(&claim).err(),
            Some("terminal is not in the expected agent input mode; open Terminal".into())
        );

        // The agent is on the pane's console when the pane's supervisor (71) started it.
        let mut fake = Fake::new(|_| Ok(serde_json::to_value(details()).unwrap()));
        fake.parents = BTreeMap::from([(72, 71), (73, 5)]);
        let backend = HostBackend::new(fake);
        assert!(backend.on_terminal("72", "conpty:%4").unwrap());
        assert!(!backend.on_terminal("73", "conpty:%4").unwrap());
        assert!(!backend.on_terminal("74", "conpty:%4").unwrap());
        assert!(!backend.on_terminal("72", "/dev/pts/4").unwrap());
        assert!(!backend.on_terminal("x", "conpty:%4").unwrap());
        assert_eq!(
            calls(&backend)[0],
            Op::PaneInfo {
                target: protocol::Target::Pane("%4".into())
            }
        );
    }

    fn spec(env: &[&str], command: &[&str], detached: bool) -> LaunchSpec {
        LaunchSpec {
            name: "fake_agent/project/one".into(),
            cwd: r"C:\work\project".into(),
            env: env.iter().map(|value| value.to_string()).collect(),
            command: command.iter().map(|word| word.to_string()).collect(),
            detached,
        }
    }

    #[test]
    fn a_session_starts_from_this_environment_without_the_enclosing_session() {
        let client = Environment::parse([
            r"Path=C:\Windows",
            "=C:=C:\\work",
            "TMUX=x",
            "TMUX_PANE=%1",
            "HGS_HOST_PANE=%9",
            "HGS_HOST_SESSION=$9",
            "HGS_SESSION=claude/outer/main",
            "hgs_run_id=outer-run",
            "HGS_TRACKING=1",
            "HGS_ACCOUNT_ID=outer-profile",
            "HGS_LAUNCH_ID=outer-launch",
            "HGS_FORK_PARENT_ID=outer-parent",
            "CLAUDECODE=1",
            "HGS_STATE_DIR=C:\\state",
            "ANTHROPIC_API_KEY=secret",
            "CLAUDE_CONFIG_DIR=C:\\claude",
            "HGS_EXECUTABLE=C:\\portable\\hgs.exe",
        ])
        .unwrap();
        let environment = session_environment(
            client,
            &[
                "HGS_CLIENT=local".into(),
                "HGS_LAUNCH_ID=0b6f4c9e-3c1d-4b8f-9a35-6f2e2d6f6a10".into(),
                "CLAUDE_CONFIG_DIR=C:\\accounts\\work".into(),
                "ANTHROPIC_API_KEY=".into(),
                "PATH=C:\\bin".into(),
            ],
            r"C:\Users\u\.local\bin\hgs.exe",
        )
        .unwrap();
        assert_eq!(
            environment,
            [
                r"Path=C:\bin",
                "=C:=C:\\work",
                "HGS_STATE_DIR=C:\\state",
                "CLAUDE_CONFIG_DIR=C:\\accounts\\work",
                r"HGS_EXECUTABLE=C:\Users\u\.local\bin\hgs.exe",
                "HGS_CLIENT=local",
                "HGS_LAUNCH_ID=0b6f4c9e-3c1d-4b8f-9a35-6f2e2d6f6a10",
            ]
        );
        // Messages name a malformed variable, never a value.
        let error = session_environment(
            Environment::default(),
            &["=secret".into(), "A=1".into()],
            r"C:\hgs.exe",
        )
        .unwrap_err();
        assert_eq!(error, "session variable 0 is not NAME=VALUE");
        assert!(session_environment(Environment::default(), &["NOVALUE".into()], "h").is_err());
    }

    #[test]
    fn agent_hint_snapshot_replaces_stale_controls_without_changing_config_paths() {
        let client = Environment::parse([
            "HGS_AGENT_HINT=0",
            "HGS_AGENT_HINT_OFF=native-claude",
            r"HGS_CONFIG_DIR=C:\private\unchanged",
            "HGS_FORK_AGENT_HINT_REVISION=1",
        ])
        .unwrap();
        let environment = session_environment(
            client,
            &["HGS_AGENT_HINT=1".into(), "HGS_AGENT_HINT_OFF=".into()],
            r"C:\stable\hgs.exe",
        )
        .unwrap();
        assert!(environment.contains(&"HGS_AGENT_HINT=1".into()));
        assert!(!environment
            .iter()
            .any(|entry| entry.starts_with("HGS_AGENT_HINT_OFF=")));
        assert!(environment.contains(&r"HGS_CONFIG_DIR=C:\private\unchanged".into()));
        assert!(!environment
            .iter()
            .any(|entry| entry.starts_with("HGS_FORK_AGENT_HINT_REVISION=")));
    }

    #[test]
    fn agent_hint_raw_capture_exception_requires_a_complete_valid_replacement_pair() {
        let pair = |entries: &[&str]| {
            entries
                .iter()
                .map(|entry| (*entry).into())
                .collect::<Vec<_>>()
        };
        assert!(resolved_hint_snapshot(&pair(&[
            "HGS_AGENT_HINT=0",
            "HGS_AGENT_HINT_OFF="
        ])));
        assert!(resolved_hint_snapshot(&pair(&[
            "hgs_agent_hint=1",
            "hgs_agent_hint_off=work"
        ])));
        for entries in [
            vec!["HGS_AGENT_HINT=0"],
            vec!["HGS_AGENT_HINT_OFF="],
            vec!["HGS_AGENT_HINT=maybe", "HGS_AGENT_HINT_OFF="],
            vec!["HGS_AGENT_HINT=1", "HGS_AGENT_HINT_OFF=bad\0value"],
            vec![
                "HGS_AGENT_HINT=0",
                "HGS_AGENT_HINT_OFF=",
                "HGS_AGENT_HINT=invalid",
            ],
        ] {
            assert!(!resolved_hint_snapshot(&pair(&entries)));
        }
    }
    #[test]
    fn creates_carry_the_launch_id_that_a_retry_repeats() {
        let command = [r"C:\Users\u\.local\bin\hgs.exe", "--run", "0", "fake_agent"];
        let request = create_request(
            &spec(
                &[
                    "HGS_CLIENT=local",
                    "HGS_LAUNCH_ID=0b6f4c9e-3c1d-4b8f-9a35-6f2e2d6f6a10",
                ],
                &command,
                true,
            ),
            vec!["A=1".into()],
            SIZE,
            || unreachable!("a launch ID was given"),
        );
        assert_eq!(
            request,
            CreateSession {
                launch_id: "0b6f4c9e-3c1d-4b8f-9a35-6f2e2d6f6a10".into(),
                name: "fake_agent/project/one".into(),
                cwd: r"C:\work\project".into(),
                env: vec!["A=1".into()],
                argv: command.iter().map(|word| word.to_string()).collect(),
                size: Size { cols: 80, rows: 24 },
            }
        );
        let request = create_request(
            &spec(&["HGS_LAUNCH_ID="], &command, true),
            Vec::new(),
            Size {
                cols: 120,
                rows: 40,
            },
            || "made-up".into(),
        );
        assert_eq!(request.launch_id, "hgs-start-made-up");
        assert_eq!(
            request.size,
            Size {
                cols: 120,
                rows: 40
            }
        );
        assert_eq!(
            listed_launch_id("hgs-start-0b6f4c9e-3c1d-4b8f-9a35-6f2e2d6f6a10"),
            ""
        );
        assert_eq!(listed_launch_id("0b6f4c9e3c1d4b8f9a356f2e2d6f6a10"), "");
        assert_eq!(listed_launch_id(""), "");

        let backend = HostBackend::new(Fake::new(|_| {
            Ok(
                json!({"session": "$0", "pane": "%0", "name": "fake_agent/project/one", "pid": 7, "existing": false}),
            )
        }));
        backend
            .start(&spec(&["HGS_CLIENT=local"], &command, true))
            .unwrap();
        let sent = requests(&backend);
        let [(true, Op::CreateSession(request))] = sent.as_slice() else {
            panic!("one create that may start a host: {sent:?}");
        };
        assert!(
            request.launch_id.starts_with("hgs-start-"),
            "{}",
            request.launch_id
        );
        assert!(request
            .env
            .contains(&r"HGS_EXECUTABLE=C:\Users\u\.local\bin\hgs.exe".into()));
        assert!(request.env.contains(&"HGS_CLIENT=local".into()));
        assert!(!request
            .env
            .iter()
            .any(|entry| entry.starts_with("HGS_SESSION=")));
        assert_eq!(
            backend
                .launch(&spec(&[], &command, true))
                .map(|exit| (exit.code, exit.server_died))
                .ok(),
            Some((0, false))
        );
        let failing = HostBackend::new(Fake::new(|_| {
            Err(host_error(
                ErrorCode::Exists,
                "duplicate session: fake_agent/project/one",
            ))
        }));
        let error = failing.launch(&spec(&[], &command, true)).err().unwrap();
        assert_eq!(
            (error.code, error.message.as_str()),
            (1, "duplicate session: fake_agent/project/one")
        );
    }

    #[test]
    fn a_launch_fails_when_its_session_ends_at_once_with_a_failure() {
        let state = |dead, ended, code: Option<i32>, output: &str| LaunchState {
            session: "$0".into(),
            pane: "%0".into(),
            name: "claude/p/a".into(),
            dead,
            ended,
            code,
            output: output.into(),
        };
        let soon = Duration::from_millis(100);
        let running = state(false, false, None, "");
        assert_eq!(startup(&running, soon, STARTUP), Startup::Wait);
        assert_eq!(startup(&running, STARTUP, STARTUP), Startup::Started);
        // A root process that has exited waits for the session's end, which
        // tells how; for at most ENDING past the startup time.
        let exited = state(true, false, None, "");
        assert_eq!(startup(&exited, STARTUP + soon, STARTUP), Startup::Wait);
        assert_eq!(
            startup(&exited, STARTUP + ENDING, STARTUP),
            Startup::Started
        );
        // Exit 0 at once has started, as a tmux launch of such a program.
        let quick = state(true, true, Some(0), "done");
        assert_eq!(startup(&quick, soon, STARTUP), Startup::Started);
        // A failure, or an exit nobody could read, fails with the pane's end.
        let denied = state(
            true,
            true,
            Some(126),
            "hgs: claude: Access is denied. (os error 5)",
        );
        assert_eq!(
            startup(&denied, soon, STARTUP),
            Startup::Failed(
                "session claude/p/a ended immediately (exit 126):\n\
                 hgs: claude: Access is denied. (os error 5)"
                    .into()
            )
        );
        assert_eq!(
            startup(&state(true, true, None, ""), STARTUP + soon, STARTUP),
            Startup::Failed("session claude/p/a ended immediately (exit code unknown)".into())
        );
        // A supervisor that could not run at all: a missing DLL.
        assert_eq!(
            startup(
                &state(true, true, Some(0xC000_0135_u32 as i32), ""),
                soon,
                STARTUP
            ),
            Startup::Failed("session claude/p/a ended immediately (exit 0xc0000135)".into())
        );
    }

    #[test]
    fn a_detached_launch_asks_how_its_session_is_doing() {
        let command = [r"C:\hgs.exe", "--run", "0", "x"];
        let created = json!({"session": "$0", "pane": "%0", "name": "fake_agent/project/one",
            "pid": 7, "existing": false});
        let ended = json!({"session": "$0", "pane": "%0", "name": "fake_agent/project/one",
            "dead": true, "ended": true, "code": 127, "output": "hgs: x: program not found"});
        let backend = HostBackend::new(Fake::new(move |op| match op {
            Op::CreateSession(_) => Ok(created.clone()),
            Op::LaunchStatus { .. } => Ok(ended.clone()),
            op => panic!("unexpected {op:?}"),
        }));
        let error = backend
            .launch(&spec(
                &["HGS_LAUNCH_ID=0b6f4c9e-3c1d-4b8f-9a35-6f2e2d6f6a10"],
                &command,
                true,
            ))
            .err()
            .unwrap();
        assert_eq!(
            (error.code, error.message.as_str()),
            (
                1,
                "session fake_agent/project/one ended immediately (exit 127):\n\
                 hgs: x: program not found"
            )
        );
        let sent = calls(&backend);
        assert!(
            matches!(&sent[1], Op::LaunchStatus { launch_id }
                if launch_id == "0b6f4c9e-3c1d-4b8f-9a35-6f2e2d6f6a10"),
            "{sent:?}"
        );
        // A start (resume, restore, fork) does not ask.
        backend.start(&spec(&[], &command, true)).unwrap();
        assert!(matches!(calls(&backend).as_slice(), [Op::CreateSession(_)]));

        // A host of an earlier version cannot tell: the launch has started, as
        // before; so has a retried launch that found its session.
        for (status, existing) in [
            (Err(ErrorCode::Unsupported), false),
            (Err(ErrorCode::NotFound), false),
            (Ok(()), true),
        ] {
            let backend = HostBackend::new(Fake::new(move |op| match op {
                Op::CreateSession(_) => Ok(json!({"session": "$0", "pane": "%0",
                    "name": "fake_agent/project/one", "pid": 7, "existing": existing})),
                Op::LaunchStatus { .. } => match status {
                    Err(code) => Err(host_error(
                        code,
                        "this session host does not know the request",
                    )),
                    Ok(()) => panic!("an existing session is not asked about"),
                },
                op => panic!("unexpected {op:?}"),
            }));
            let exit = backend.launch(&spec(&[], &command, true)).ok().unwrap();
            assert_eq!((exit.code, exit.server_died), (0, false));
        }
    }

    #[test]
    fn without_a_console_attaching_is_refused_before_anything_starts() {
        let backend = HostBackend::new(Fake::done());
        for result in [
            backend
                .launch(&spec(&[], &[r"C:\hgs.exe", "--run", "0", "x"], false))
                .map(|exit| exit.code),
            backend
                .attach(Target::Name("x/p"), Some("run-1"))
                .map(|exit| exit.code),
            backend.attachable().map(|()| 0),
        ] {
            let error = result.unwrap_err();
            assert_eq!((error.code, error.message.as_str()), (1, NOT_A_CONSOLE));
        }
        assert!(requests(&backend).is_empty());
    }

    #[test]
    fn a_dry_run_masks_a_secret_in_the_launch_environment() {
        let launch = spec(
            &["ANTHROPIC_AUTH_TOKEN=sk-secret", "ANTHROPIC_BASE_URL=http://192.0.2.10:8080"],
            &[r"C:\hgs.exe", "--run", "0", "claude"],
            false,
        );
        let line = describe(&DryRun::Launch(&launch));
        assert!(line.contains("ANTHROPIC_AUTH_TOKEN=***"), "{line}");
        assert!(!line.contains("sk-secret"), "the secret never reaches a dry-run: {line}");
        assert!(line.contains("ANTHROPIC_BASE_URL=http://192.0.2.10:8080"), "{line}");
    }

    #[test]
    fn dry_runs_describe_the_host_requests() {
        assert_eq!(describe(&DryRun::ListJson), "session-host list");
        assert_eq!(describe(&DryRun::List), "session-host list");
        assert_eq!(
            describe(&DryRun::Attach {
                session: "claude/my project/t",
                client: "local"
            }),
            "session-host set-environment -t \"claude/my project/t\" HGS_CLIENT local\n\
             session-host attach -t \"claude/my project/t\""
        );
        assert_eq!(
            describe(&DryRun::AttachExisting {
                session: "claude/p/t",
                run_id: Some("run 1")
            }),
            "session-host attach existing session claude/p/t (run \"run 1\")"
        );
        assert_eq!(
            describe(&DryRun::AttachExisting {
                session: "claude/p/t",
                run_id: None
            }),
            "session-host attach existing session claude/p/t"
        );
        assert_eq!(
            describe(&DryRun::Kill {
                session: "claude/p/t"
            }),
            "session-host kill -t claude/p/t"
        );
        let launch = spec(
            &["HGS_CLIENT=local", "B=two words"],
            &[
                r"C:\Users\a b\.local\bin\hgs.exe",
                "--run",
                "0",
                "claude",
                "say \"hi\"",
            ],
            false,
        );
        assert_eq!(
            describe(&DryRun::Launch(&launch)),
            "session-host create-session -s fake_agent/project/one -c C:\\work\\project \
             -e HGS_CLIENT=local -e \"B=two words\" -- \"C:\\Users\\a b\\.local\\bin\\hgs.exe\" \
             --run 0 claude \"say \\\"hi\\\"\""
        );
        let detached = LaunchSpec {
            detached: true,
            env: Vec::new(),
            ..spec(&[], &[r"C:\hgs.exe", "--run", "0", "x"], false)
        };
        let line = r"session-host create-session -d -s fake_agent/project/one -c C:\work\project -- C:\hgs.exe --run 0 x";
        assert_eq!(describe(&DryRun::Launch(&detached)), line);
        // A relaunch is always detached.
        let relaunch = LaunchSpec {
            detached: false,
            ..detached
        };
        assert_eq!(describe(&DryRun::Start(&relaunch)), line);
        assert_eq!(
            HostBackend::new(Fake::done()).describe(&DryRun::List),
            "session-host list"
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_backend_describes_without_a_host() {
        assert_eq!(HOST.describe(&DryRun::List), "session-host list");
    }
}
