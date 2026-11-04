//! The terminal that hosts agent sessions: tmux on Unix, the session host on a
//! Windows computer with local sessions enabled (`host`, spec D2-D5, D8). Each
//! session holds one pane, addressed by its exact name, its immutable session ID
//! (`$N`) or its pane ID (`%N`); records, rename transactions and question hashes
//! keep those forms.
//!
//! The tmux backend issues exactly the commands hgs always has, in the same order,
//! with the same arguments, stdin and stdio. Only on Linux, where hgs runs in a
//! user unit, a `new-session` first checks for a server (`list-sessions`); when
//! none answers, it runs in a scope of its own (`tmux::scope`), so that stopping
//! that unit leaves the agents running. Answers keep the terminal's own text
//! where callers validate it, so their checks and messages stay where they were.
//! Operations a backend lacks report local sessions as unavailable, as a Windows
//! computer whose local sessions are not enabled does for all of them.
// Only the tmux backend reads these types on Unix; the host backend is compiled
// everywhere so that its mapping is tested on every platform.
#![cfg_attr(not(unix), allow(dead_code))]

pub(crate) mod host;
#[cfg(unix)]
mod tmux;

use super::Result;
use crate::cli::{Error, Result as CliResult};

/// What an operation reports on a computer without a session terminal, as every
/// other local session command there does. The tmux and host backends implement
/// every operation, so only that computer's backend reports it.
const UNSUPPORTED: &str = crate::platform::LOCAL_SESSIONS_UNSUPPORTED;
/// What `hgs a --existing --run-id` reports when the session runs another run.
pub(crate) const RUN_CHANGED: &str = "session run changed; refresh before reconnecting";

/// User-visible text that names the session terminal. Where tmux hosts the
/// sessions (Unix) it keeps tmux's words byte for byte; on Windows it says the
/// same without them, as the session host is not named to its users.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Wording {
    tmux: &'static str,
    neutral: &'static str,
}
impl Wording {
    /// The text where tmux hosts the sessions (`tmux`) or where it does not.
    pub(crate) const fn on(self, tmux: bool) -> &'static str {
        if tmux {
            self.tmux
        } else {
            self.neutral
        }
    }
    /// The text for this computer's session terminal.
    pub(crate) const fn text(self) -> &'static str {
        self.on(cfg!(unix))
    }
}
/// `hgs a --existing` and `hgs rename`: the terminal named no `$N` session ID.
pub(crate) const INVALID_SESSION_ID: Wording = Wording {
    tmux: "invalid tmux session identity",
    neutral: "invalid session identity",
};
/// Rename recovery: the renamed session now has neither the old nor the new name.
pub(crate) const RENAME_RECOVERY_CHANGED: Wording = Wording {
    tmux: "tmux session changed during rename recovery",
    neutral: "session changed during rename recovery",
};
/// A session's `terminal_reason` while it is not its exact live tracked pane.
pub(crate) const TERMINAL_NEEDS_PANE: Wording = Wording {
    tmux: "Terminal requires this exact live tracked tmux pane",
    neutral: "Terminal requires this exact live tracked pane",
};
/// `hgs terminal` of an OpenCode session.
pub(crate) const OPENCODE_TERMINAL: Wording = Wording {
    tmux: "OpenCode uses its native API without a tmux Terminal",
    neutral: "OpenCode uses its native API without a Terminal",
};
/// An OpenCode session's `terminal_reason`.
pub(crate) const OPENCODE_TERMINAL_REASON: Wording = Wording {
    tmux: "OpenCode uses its native API; this adapter has no tmux Terminal",
    neutral: "OpenCode uses its native API; this adapter has no Terminal",
};
/// `hgs terminal` of a DeepSeek session and its `terminal_reason`.
pub(crate) const DEEPSEEK_TERMINAL: Wording = Wording {
    tmux: "DeepSeek uses a structured native API without a tmux Terminal",
    neutral: "DeepSeek uses a structured native API without a Terminal",
};

/// What a command acts on.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Target<'a> {
    /// The session with exactly this name.
    Name(&'a str),
    /// An immutable session ID, `$N`.
    Session(&'a str),
    /// A pane ID, `%N`.
    Pane(&'a str),
}

/// A listed client terminal and the session it shows.
pub(crate) struct ClientRow {
    pub session: String,
    pub terminal: String,
}

/// A listed session. Counts that cannot be read are 0.
pub(crate) struct SessionRow {
    pub name: String,
    /// Its immutable session ID, `$N`, which a rename keeps.
    pub id: String,
    pub attached: u64,
    pub created: u64,
    pub launch_id: String,
}

/// The `hgs ls --json` view of the terminal: clients, then sessions.
pub(crate) struct Listing {
    pub clients: Vec<ClientRow>,
    pub sessions: Vec<SessionRow>,
}

/// A live pane's identity: `dead` is "1" once its process has exited and
/// `run_marker` is the run ID its supervisor set ("" when untracked).
pub(crate) struct PaneRow {
    pub session: String,
    /// The session's immutable ID, `$N`.
    pub session_id: String,
    pub pane: String,
    pub dead: String,
    pub run_marker: String,
}

/// A pane's reported current directory (passive metadata only).
pub(crate) struct PanePath {
    pub session: String,
    /// The session's immutable ID, `$N`.
    pub session_id: String,
    pub active_window: bool,
    pub active_pane: bool,
    pub path: String,
}

/// One fact about a pane, as the terminal reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneField {
    SessionId,
    SessionName,
    SocketPath,
    SessionCreated,
    SessionWindows,
    SessionGrouped,
    WindowPanes,
    WindowLinked,
    PaneId,
    /// The terminal server's process ID.
    ServerPid,
    /// The pane's root process: the `hgs --run` supervisor.
    PanePid,
    Width,
    Height,
    /// In cells.
    CursorX,
    CursorY,
    /// "0" unless a copy or view mode holds the pane.
    InMode,
    CurrentCommand,
    Dead,
    RunMarker,
    /// "1" when the application enabled bracketed paste.
    BracketedPaste,
    Tty,
}

/// The requested pane facts in request order, as reported.
pub(crate) struct PaneInfo {
    text: String,
}
impl PaneInfo {
    pub(crate) fn new(text: String) -> Self {
        Self { text }
    }
    /// The answer to a one-field request, without surrounding whitespace.
    pub(crate) fn value(&self) -> &str {
        self.text.trim()
    }
    /// Every requested field. A value that holds a tab makes the count differ.
    pub(crate) fn fields(&self) -> Vec<&str> {
        self.text.trim_end_matches('\n').split('\t').collect()
    }
    /// The answer to a two-field request.
    pub(crate) fn pair(&self) -> Option<(&str, &str)> {
        self.text.trim().split_once('\t')
    }
}

/// The visible screen, one line per row with trailing spaces trimmed. `Styled`
/// keeps SGR attributes.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Capture {
    Plain,
    Styled,
}

/// Input for a pane: a named key (`Enter`, `Up`, `C-u`, `M-s`) or literal text.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Key<'a> {
    Named(&'a str),
    Literal(&'a str),
}

/// How staged text is pasted. Line feeds are kept in both forms.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Paste {
    /// An agent composer: inside bracketed-paste markers when the application
    /// enabled them.
    Composer,
    /// Raw Terminal input: markers only when the caller saw bracketed paste on.
    Terminal { bracketed: bool },
}

/// What staging does with the terminal's own error output. Neither form shows
/// it: message input reads it to the end, other staging discards it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StageErrors {
    Read,
    Discard,
}

/// The pinned agent that should own a pane's input.
pub(crate) struct InputClaim<'a> {
    pub pane: &'a str,
    pub name: &'a str,
    pub run_id: &'a str,
    pub agent: &'a str,
    pub pid: Option<u64>,
}

/// A pane whose agent owns its input now; the cursor is in cells, as reported.
pub(crate) struct InputState {
    pub cursor_x: String,
    pub cursor_y: String,
}

/// A new session. `env` holds `KEY=VALUE` overrides and `command` the words of
/// the pane's command line, which the tmux backend runs through the shell and
/// the session host as the root process's argv (spec D8).
pub(crate) struct LaunchSpec {
    pub name: String,
    pub cwd: String,
    pub env: Vec<String>,
    pub command: Vec<String>,
    /// For a CLI launch: return at once instead of attaching this terminal.
    pub detached: bool,
}

/// Environment variables whose value is a secret: `--dry-run` prints the name
/// and `***`, never the value, even if a caller puts one in a `LaunchSpec`
/// (U60 #658 N3).
pub(crate) const SECRET_ENV: &[&str] = &[
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "CODEX_ACCESS_TOKEN",
    "HGS_MODEL_SERVER_KEY",
    "OPENCODE_CONFIG_CONTENT",
    "KIMI_API_KEY",
    "MOONSHOT_API_KEY",
    "CLAUDE_CODE_OAUTH_TOKEN",
];

/// One `KEY=VALUE` for a dry-run: a secret's value is masked.
pub(crate) fn masked(value: &str) -> String {
    match value.split_once('=') {
        Some((key, _)) if SECRET_ENV.contains(&key) => format!("{key}=***"),
        _ => value.to_owned(),
    }
}

/// How an interactive client ended: its exit code, and whether the terminal
/// server died (the terminal is then already restored and the user told).
pub(crate) struct ClientExit {
    pub code: i32,
    pub server_died: bool,
}

/// What a `--dry-run` would have done.
pub(crate) enum DryRun<'a> {
    ListJson,
    List,
    Attach {
        session: &'a str,
        client: &'a str,
    },
    AttachExisting {
        session: &'a str,
        run_id: Option<&'a str>,
    },
    Kill {
        session: &'a str,
    },
    /// A session started from the CLI.
    Launch(&'a LaunchSpec),
    /// A saved session relaunched detached (resume, restore, fresh, fork).
    Start(&'a LaunchSpec),
}

/// The session terminal. Without a server there are no sessions: listings come
/// back empty. A listing that fails for any other reason is an error, never an
/// empty answer, which would show live sessions as stopped and resumable (spec
/// D3). Methods returning a `CliResult` serve an interactive CLI command and
/// leave the terminal's own messages on the user's terminal.
pub(crate) trait TerminalBackend: Sync {
    /// Whether the terminal server runs.
    fn server_running(&self) -> bool {
        false
    }
    /// Whether this process runs inside a session pane.
    fn inside(&self) -> bool {
        false
    }
    /// The name of the session this process runs in ("" when unknown).
    fn enclosing_session(&self) -> String {
        String::new()
    }
    /// The pane a `--run` supervisor was started in.
    fn own_pane(&self) -> Result<String> {
        Err(UNSUPPORTED.into())
    }
    /// Whether the terminal replaces the last lines in the report that a
    /// `--run` supervisor leaves for its launch (`launches::finished`) with
    /// those its session showed once the session's output had drained.
    fn completes_launch_reports(&self) -> bool {
        false
    }

    fn listing(&self) -> Result<Listing> {
        Ok(Listing {
            clients: Vec::new(),
            sessions: Vec::new(),
        })
    }
    /// `(name, attached clients)` for `hgs ls`; None without sessions.
    fn attached_counts(&self) -> Result<Option<Vec<(String, String)>>> {
        Ok(None)
    }
    fn panes(&self) -> Result<Vec<PaneRow>> {
        Err(UNSUPPORTED.into())
    }
    fn pane_paths(&self) -> Result<Vec<PanePath>> {
        Err(UNSUPPORTED.into())
    }

    fn session_exists(&self, _name: &str) -> bool {
        false
    }
    /// The current name of session `$N`, None when it no longer exists.
    fn session_name(&self, _id: &str) -> Result<Option<String>> {
        Err(UNSUPPORTED.into())
    }
    fn pane_info(&self, _target: Target, _fields: &[PaneField]) -> Result<PaneInfo> {
        Err(UNSUPPORTED.into())
    }
    /// `pane_info` where only the answer matters: None when the terminal gives
    /// none, its message discarded.
    fn probe(&self, _target: Target, _fields: &[PaneField]) -> Option<PaneInfo> {
        None
    }
    /// Start a detached session.
    fn start(&self, _spec: &LaunchSpec) -> Result<()> {
        Err(UNSUPPORTED.into())
    }
    /// Mark session `name` with the run ID of the supervisor in its pane.
    fn set_run_marker(&self, _name: &str, _run_id: &str) -> Result<()> {
        Err(UNSUPPORTED.into())
    }
    /// Rename session `$N`.
    fn rename(&self, _id: &str, _name: &str) -> Result<()> {
        Err(UNSUPPORTED.into())
    }
    fn kill_session(&self, _target: Target) -> Result<()> {
        Err(UNSUPPORTED.into())
    }
    /// Remove an exited pane.
    fn kill_pane(&self, _pane: &str) -> Result<()> {
        Err(UNSUPPORTED.into())
    }

    /// Ask the tracked agent `pid` in `pane` to exit, as a pause does: SIGTERM to
    /// that exact process on Unix; on Windows the pane's supervisor raises
    /// Ctrl+Break for the process group it shares with its agent (spec D11).
    /// Never a forced kill.
    fn request_exit(&self, _pane: &str, pid: u32) -> Result<()> {
        crate::platform::request_exit(pid).map_err(|error| error.to_string())
    }

    /// Check that the claimed agent owns the pane's input now: the pane shows its
    /// run, is in normal mode with bracketed paste on, the terminal is raw and
    /// the agent, not a shell, is in the foreground.
    fn input_state(&self, _claim: &InputClaim) -> Result<InputState> {
        Err(UNSUPPORTED.into())
    }
    /// Whether process `pid` runs on `tty`, a pane's reported `Tty`.
    fn on_terminal(&self, _pid: &str, _tty: &str) -> Result<bool> {
        Err(UNSUPPORTED.into())
    }
    fn capture(&self, _pane: &str, _mode: Capture) -> Result<Vec<u8>> {
        Err(UNSUPPORTED.into())
    }
    /// Make the window of `pane` `rows` tall, keeping its width, only while no
    /// client views it and no window option of its own fixes its size. The
    /// window still takes the size of the next client that attaches. False when
    /// the size stays as it was.
    fn grow_unviewed(&self, _pane: &str, _rows: u16) -> Result<bool> {
        Ok(false)
    }
    /// Send keys in order. An empty list still checks that the pane exists.
    fn send(&self, _pane: &str, _keys: &[Key]) -> Result<()> {
        Err(UNSUPPORTED.into())
    }
    /// Stage text under a unique name without placing it on argv. Err when the
    /// terminal could not be run, false when it did not take the text.
    fn stage(&self, _buffer: &str, _text: &[u8], _errors: StageErrors) -> Result<bool> {
        Err(UNSUPPORTED.into())
    }
    /// Paste and release staged text.
    fn paste(&self, _buffer: &str, _pane: &str, _mode: Paste) -> Result<()> {
        Err(UNSUPPORTED.into())
    }
    /// Release staged text if it is still there.
    fn discard(&self, _buffer: &str) {}

    /// Start a session from the CLI, attached unless `spec.detached`.
    fn launch(&self, _spec: &LaunchSpec) -> CliResult<ClientExit> {
        Err(Error::new(2, UNSUPPORTED))
    }
    /// Refuse an attach this process cannot make (`hgs a`, a launch without
    /// `-d`) before a saved session is resumed for it; tmux finds out as it
    /// attaches.
    fn attachable(&self) -> CliResult<()> {
        Ok(())
    }
    /// Record which peer views the session, for `hgs paste`.
    fn set_client(&self, _target: Target, _client: &str) {}
    /// Attach this terminal to a session. With `run_id` (`hgs a --existing
    /// --run-id`, checked by the caller before) a terminal that can check the
    /// run marker again as it attaches refuses a changed run with `RUN_CHANGED`.
    fn attach(&self, _target: Target, _run_id: Option<&str>) -> CliResult<ClientExit> {
        Err(Error::new(2, UNSUPPORTED))
    }
    /// `hgs kill`: end the session named exactly `name`; the exit code.
    fn kill(&self, _name: &str) -> CliResult<i32> {
        Err(Error::new(2, UNSUPPORTED))
    }
    /// Retitle the terminals attached to session `name` after a rename.
    fn refresh_titles(&self, _name: &str) -> CliResult<()> {
        Ok(())
    }
    /// The viewing peer recorded by `set_client`, as stored.
    fn viewer(&self, _name: &str) -> String {
        String::new()
    }
    /// Show a short message on a client terminal.
    fn notify(&self, _client: &str, _text: &str) {}
    /// Hand a key to the pane whatever happened before (the paste binding).
    fn forward_key(&self, _pane: &str, _key: &str) {}
    fn describe(&self, _operation: &DryRun) -> String {
        UNSUPPORTED.into()
    }
}

/// A computer without a session terminal: a Windows computer whose local
/// sessions are not enabled (spec D2), a control console for remote machines.
/// No host runs for it, so it has no live sessions: listings are empty, which
/// leaves saved sessions stopped, and every action fails.
#[cfg(not(unix))]
struct Unavailable;
#[cfg(not(unix))]
impl TerminalBackend for Unavailable {
    fn panes(&self) -> Result<Vec<PaneRow>> {
        Ok(Vec::new())
    }
    fn pane_paths(&self) -> Result<Vec<PanePath>> {
        Ok(Vec::new())
    }
    fn session_name(&self, _id: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

#[cfg(unix)]
static BACKEND: tmux::Tmux = tmux::Tmux;
#[cfg(not(unix))]
static BACKEND: Unavailable = Unavailable;

#[cfg(test)]
thread_local! {
    static FAKE: std::cell::Cell<Option<&'static dyn TerminalBackend>> =
        const { std::cell::Cell::new(None) };
}

/// This computer's session terminal. On Windows it is the session host once
/// local sessions are enabled, decided again on every call as
/// `platform::local_sessions` is. Unit tests never reach a host: they get the
/// unavailable backend or the fake they install.
pub(crate) fn current() -> &'static dyn TerminalBackend {
    #[cfg(test)]
    if let Some(fake) = FAKE.with(std::cell::Cell::get) {
        return fake;
    }
    #[cfg(all(windows, not(test)))]
    if crate::platform::local_sessions() {
        return &host::HOST;
    }
    &BACKEND
}

/// Run `body` with `fake` as this thread's backend.
#[cfg(test)]
pub(crate) fn with_fake<T>(fake: &'static dyn TerminalBackend, body: impl FnOnce() -> T) -> T {
    struct Reset(Option<&'static dyn TerminalBackend>);
    impl Drop for Reset {
        fn drop(&mut self) {
            FAKE.with(|cell| cell.set(self.0));
        }
    }
    let _reset = Reset(FAKE.with(|cell| cell.replace(Some(fake))));
    body()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::state::{storage, workspace};
    use serde_json::json;

    struct Fake;
    impl TerminalBackend for Fake {
        fn panes(&self) -> Result<Vec<PaneRow>> {
            let row = |session: &str, id: &str, pane: &str, dead: &str, run: &str| PaneRow {
                session: session.into(),
                session_id: id.into(),
                pane: pane.into(),
                dead: dead.into(),
                run_marker: run.into(),
            };
            Ok(vec![
                row("claude/p/a", "$1", "%1", "0", "run-a"),
                row("codex/p/b", "$2", "%2", "0", "run-b"),
                row("codex/p/b", "$2", "%3", "1", ""),
            ])
        }
        fn pane_paths(&self) -> Result<Vec<PanePath>> {
            let path = |session: &str, id: &str, active_window, active_pane, path: &str| PanePath {
                session: session.into(),
                session_id: id.into(),
                active_window,
                active_pane,
                path: path.into(),
            };
            Ok(vec![
                path("codex/p/b", "$2", false, true, "/inactive-window"),
                path("codex/p/b", "$2", true, true, "/active"),
                path("codex/p/b", "$2", true, false, "/inactive-pane"),
                path("codex/p/b", "$2", true, true, "relative"),
                // Renamed since `panes`, started since, and without an ID.
                path("claude/p/renamed", "$1", true, true, "/renamed"),
                path("claude/p/new", "$3", true, true, "/new"),
                path("sh/p/plain", "", true, true, "/plain"),
            ])
        }
    }

    #[test]
    fn session_state_reads_the_current_backend() {
        with_fake(&Fake, || {
            let snapshot = storage::live().unwrap();
            let record = json!({"pane": "%1", "run_id": "run-a"});
            assert!(storage::matches(&record, snapshot.get("claude/p/a")));
            // One session holds exactly one pane.
            let record = json!({"pane": "%2", "run_id": "run-b"});
            assert!(!storage::matches(&record, snapshot.get("codex/p/b")));
            assert_eq!(workspace::pane_cwds()["codex/p/b"], "/active");
            // The same answer names each live session by its immutable ID.
            let (sessions, names) = storage::live_sessions().unwrap();
            assert_eq!(sessions.len(), snapshot.len());
            // Folders read later keep the names those IDs had then.
            assert_eq!(
                workspace::snapshot_cwds(&names)
                    .into_iter()
                    .collect::<Vec<_>>(),
                [
                    ("claude/p/a".into(), "/renamed".into()),
                    ("codex/p/b".into(), "/active".into()),
                    ("sh/p/plain".into(), "/plain".into())
                ]
            );
            assert_eq!(
                names.into_iter().collect::<Vec<_>>(),
                [
                    ("$1".into(), "claude/p/a".into()),
                    ("$2".into(), "codex/p/b".into())
                ]
            );
        });
        struct Empty;
        impl TerminalBackend for Empty {}
        with_fake(&Empty, || {
            assert_eq!(storage::live().unwrap_err(), UNSUPPORTED);
            assert!(workspace::pane_cwds().is_empty());
            assert!(!current().session_exists("claude/p/a"));
        });
    }
}

#[cfg(test)]
mod wording_tests {
    use super::*;

    #[test]
    fn only_tmux_is_named_and_its_words_stay_byte_for_byte() {
        for (wording, tmux, neutral) in [
            (
                INVALID_SESSION_ID,
                "invalid tmux session identity",
                "invalid session identity",
            ),
            (
                RENAME_RECOVERY_CHANGED,
                "tmux session changed during rename recovery",
                "session changed during rename recovery",
            ),
            (
                TERMINAL_NEEDS_PANE,
                "Terminal requires this exact live tracked tmux pane",
                "Terminal requires this exact live tracked pane",
            ),
            (
                OPENCODE_TERMINAL,
                "OpenCode uses its native API without a tmux Terminal",
                "OpenCode uses its native API without a Terminal",
            ),
            (
                OPENCODE_TERMINAL_REASON,
                "OpenCode uses its native API; this adapter has no tmux Terminal",
                "OpenCode uses its native API; this adapter has no Terminal",
            ),
            (
                DEEPSEEK_TERMINAL,
                "DeepSeek uses a structured native API without a tmux Terminal",
                "DeepSeek uses a structured native API without a Terminal",
            ),
        ] {
            assert_eq!(wording.on(true), tmux);
            assert_eq!(wording.on(false), neutral);
            assert_eq!(wording.text(), if cfg!(unix) { tmux } else { neutral });
        }
    }
}

#[cfg(all(test, not(unix)))]
mod unavailable_tests {
    use super::*;
    use crate::state::{storage, workspace};

    #[test]
    fn no_host_lists_no_live_sessions_and_refuses_every_action() {
        let terminal = current();
        assert!(!terminal.server_running() && !terminal.inside());
        let listing = terminal.listing().unwrap();
        assert!(listing.clients.is_empty() && listing.sessions.is_empty());
        assert!(terminal.attached_counts().unwrap().is_none());
        assert!(storage::live().unwrap().is_empty());
        assert!(workspace::pane_cwds().is_empty());
        assert!(!terminal.session_exists("claude/p"));
        assert_eq!(terminal.session_name("$1").unwrap(), None);
        assert_eq!(terminal.own_pane().unwrap_err(), UNSUPPORTED);
        assert!(!terminal.completes_launch_reports());
        assert_eq!(
            terminal.pane_info(Target::Pane("%1"), &[PaneField::Dead]).err(),
            Some(UNSUPPORTED.into())
        );
        assert!(terminal
            .probe(Target::Name("claude/p"), &[PaneField::SessionId])
            .is_none());
        assert_eq!(
            terminal.stage("hgs-input-1", b"text", StageErrors::Read).unwrap_err(),
            UNSUPPORTED
        );
        assert_eq!(terminal.capture("%1", Capture::Plain).unwrap_err(), UNSUPPORTED);
        assert_eq!(terminal.send("%1", &[Key::Named("Enter")]).unwrap_err(), UNSUPPORTED);
        assert_eq!(terminal.kill_session(Target::Name("claude/p")).unwrap_err(), UNSUPPORTED);
        for result in [
            terminal
                .attach(Target::Name("claude/p"), None)
                .map(|exit| exit.code),
            terminal.kill("claude/p"),
        ] {
            let error = result.unwrap_err();
            assert_eq!((error.code, error.message.as_str()), (2, UNSUPPORTED));
        }
    }
}
