//! The session host's protocol end to end, on every platform: a real `Host`,
//! its connection handling and the client library over in-memory links, with
//! a fake launcher whose "programs" the tests drive. The ConPTY launcher and
//! the pipe transport are covered by the Windows tests of their modules and by
//! `tests/test_windows_host.py`.
use super::{
    channel::{answer_requests, SupervisorChannel, NOT_AGENT_INPUT, NOT_ATTACHED, NOT_RAW},
    client::{ClientError, Connection, Event},
    link::{memory_pair, write_json, Link},
    protocol::{
        Attach, Attached, CallerSession, Captured, ConsoleEvidence, ConsoleProcess, Control, CreateSession,
        Created, Done, EnvValue, ErrorBody, ErrorCode, Grown, Hello, HelloAnswer, HelloReply,
        InputClaim, InputReady, Integrity, Key, Kind, LaunchState, Listed, Op, PaneDetails,
        Protocols, Resolved, Role, Signal, Size, SupervisorOp, SupervisorRegistration, Target,
        RUN_MARKER,
    },
    registry::{last_lines, Host, Linger, Next},
    server::serve_connection,
    session::{
        self, foreground, paste_bytes, Attachment, Console, LaunchPlan, Launched, Launcher, Out,
    },
    supervisor, worker,
};
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Condvar, Mutex, PoisonError,
    },
    thread,
    time::{Duration, Instant},
};

const LIMIT: Duration = Duration::from_secs(10);
const CLIENT_PID: u32 = 4242;
/// Absolute paths as the host checks them on this platform.
#[cfg(windows)]
const PROGRAM: &str = r"C:\hgs\hgs.exe";
#[cfg(windows)]
const WORK: &str = r"C:\work\project";
#[cfg(windows)]
const OTHER_WORK: &str = r"C:\work\other";
#[cfg(not(windows))]
const PROGRAM: &str = "/opt/hgs/hgs";
#[cfg(not(windows))]
const WORK: &str = "/work/project";
#[cfg(not(windows))]
const OTHER_WORK: &str = "/work/other";

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Poll `done` until it holds or `LIMIT` passes.
fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + LIMIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

/// A byte pipe like the pseudoconsole's: the reader blocks until bytes come
/// or the pipe is closed.
#[derive(Default)]
struct Bytes {
    state: Mutex<(VecDeque<u8>, bool)>,
    ready: Condvar,
}

impl Bytes {
    fn push(&self, bytes: &[u8]) -> io::Result<()> {
        let mut state = lock(&self.state);
        if state.1 {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        state.0.extend(bytes);
        self.ready.notify_all();
        Ok(())
    }

    fn close(&self) {
        lock(&self.state).1 = true;
        self.ready.notify_all();
    }

    /// Everything readable now, waiting at most `timeout` for something.
    fn take(&self, timeout: Duration) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.state);
        while state.0.is_empty() && !state.1 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            state = self
                .ready
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        state.0.drain(..).collect()
    }
}

struct Reader(Arc<Bytes>);

impl Read for Reader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut state = lock(&self.0.state);
        loop {
            if !state.0.is_empty() {
                let count = buffer.len().min(state.0.len());
                for (slot, byte) in buffer.iter_mut().zip(state.0.drain(..count)) {
                    *slot = byte;
                }
                return Ok(count);
            }
            if state.1 {
                return Ok(0);
            }
            state = self
                .0
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

struct Writer(Arc<Bytes>);

impl Write for Writer {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.push(buffer)?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One fake session's program and console.
struct Program {
    pid: u32,
    argv: Vec<String>,
    cwd: String,
    environment: Vec<(String, Option<String>)>,
    /// What the program writes to its console.
    output: Arc<Bytes>,
    /// What the host writes to the console's input.
    input: Arc<Bytes>,
    exit: Mutex<Option<u32>>,
    exited: Condvar,
    sizes: Mutex<Vec<Size>>,
    hung_up: AtomicBool,
    ended: AtomicBool,
    /// Like a program that handles CTRL_CLOSE_EVENT by exiting.
    exit_on_hang_up: AtomicBool,
    /// What the console renders as it closes: output the program wrote before
    /// it exited that reaches the host only with the pseudoconsole's last frame.
    closing: Mutex<Vec<u8>>,
    /// Processes in the session's job besides the root.
    members: Mutex<Vec<u32>>,
}

impl Program {
    fn write(&self, bytes: &[u8]) {
        self.output.push(bytes).unwrap();
    }

    /// What the host has written to the input so far, after a short wait.
    fn read_input(&self, wait: Duration) -> Vec<u8> {
        self.input.take(wait)
    }

    /// Read input until it ends with `expected`, returning all of it.
    fn input_until(&self, expected: &[u8]) -> Vec<u8> {
        let mut seen = Vec::new();
        eventually("console input", || {
            seen.extend(self.input.take(Duration::from_millis(20)));
            seen.ends_with(expected)
        });
        seen
    }

    fn exit(&self, code: u32) {
        let mut exit = lock(&self.exit);
        if exit.is_none() {
            *exit = Some(code);
        }
        self.exited.notify_all();
    }

    fn sizes(&self) -> Vec<Size> {
        lock(&self.sizes).clone()
    }
}

struct FakeConsole(Arc<Program>);

impl Console for FakeConsole {
    fn resize(&self, size: Size) -> io::Result<()> {
        lock(&self.0.sizes).push(size);
        Ok(())
    }

    fn wait(&self) -> Option<u32> {
        let mut exit = lock(&self.0.exit);
        while exit.is_none() {
            exit = self
                .0
                .exited
                .wait(exit)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *exit
    }

    fn wait_exit(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut exit = lock(&self.0.exit);
        while exit.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            exit = self
                .0
                .exited
                .wait_timeout(exit, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    fn hang_up(&self) {
        if self.0.hung_up.swap(true, Ordering::SeqCst) {
            return;
        }
        if self.0.exit_on_hang_up.load(Ordering::SeqCst) {
            self.0.exit(0xC000_013A);
        }
        let closing = std::mem::take(&mut *lock(&self.0.closing));
        if !closing.is_empty() {
            self.0.write(&closing);
        }
        // The console's output ends when it is closed.
        self.0.output.close();
    }

    fn end(&self) {
        self.0.ended.store(true, Ordering::SeqCst);
        self.0.exit(1);
        self.0.output.close();
    }

    fn current_command(&self) -> String {
        "fake".into()
    }

    fn holds(&self, pid: u32) -> bool {
        lock(&self.0.members).contains(&pid)
    }
}

#[derive(Default)]
struct Launches {
    programs: Mutex<Vec<Arc<Program>>>,
    next_pid: AtomicU32,
    fail: AtomicBool,
    /// The next programs have exited by the time their launch returns.
    exit_at_once: AtomicBool,
    /// The next program writes this and has exited with this code by the
    /// time its launch returns, as a supervisor that cannot start its agent.
    fail_at_once: Mutex<Option<(u32, Vec<u8>)>>,
    /// While set, a launch waits (`waiting` counts those that do).
    held: Mutex<bool>,
    released: Condvar,
    waiting: AtomicU32,
}

#[derive(Clone, Default)]
struct FakeLauncher(Arc<Launches>);

impl FakeLauncher {
    fn program(&self, index: usize) -> Arc<Program> {
        lock(&self.0.programs)[index].clone()
    }

    fn count(&self) -> usize {
        lock(&self.0.programs).len()
    }

    fn hold(&self, held: bool) {
        *lock(&self.0.held) = held;
        self.0.released.notify_all();
    }
}

impl Launcher for FakeLauncher {
    fn launch(&self, plan: &LaunchPlan<'_>) -> io::Result<Launched> {
        if self.0.fail.swap(false, Ordering::SeqCst) {
            return Err(io::Error::other("no such program"));
        }
        {
            let mut held = lock(&self.0.held);
            if *held {
                self.0.waiting.fetch_add(1, Ordering::SeqCst);
            }
            while *held {
                held = self
                    .0
                    .released
                    .wait(held)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
        let failure = lock(&self.0.fail_at_once).take();
        let exited = match &failure {
            Some((code, _)) => Some(*code),
            None => self.0.exit_at_once.load(Ordering::SeqCst).then_some(0),
        };
        let names = [
            "TERM",
            "COLORTERM",
            "HGS_HOST_SESSION",
            "HGS_HOST_PANE",
            "Path",
            "HGS_EXECUTABLE",
            "HGS_SESSION",
            "HGS_CLIENT",
        ];
        let program = Arc::new(Program {
            pid: 1000 + self.0.next_pid.fetch_add(1, Ordering::SeqCst),
            argv: plan.argv.to_vec(),
            cwd: plan.cwd.into(),
            environment: names
                .iter()
                .map(|name| {
                    (
                        name.to_string(),
                        plan.environment.get(name).map(String::from),
                    )
                })
                .collect(),
            output: Arc::default(),
            input: Arc::default(),
            exit: Mutex::new(exited),
            exited: Condvar::new(),
            sizes: Mutex::new(vec![plan.size]),
            hung_up: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            exit_on_hang_up: AtomicBool::new(true),
            closing: Mutex::new(Vec::new()),
            members: Mutex::new(Vec::new()),
        });
        if let Some((_, said)) = failure {
            program.write(&said);
        }
        lock(&self.0.programs).push(program.clone());
        Ok(Launched {
            pid: program.pid,
            output: Box::new(Reader(program.output.clone())),
            input: Box::new(Writer(program.input.clone())),
            console: Box::new(FakeConsole(program)),
        })
    }
}

fn identity() -> HelloReply {
    HelloReply {
        pid: 77,
        created: "133000000000000000".into(),
        image: r"C:\hgs\hgs.exe".into(),
        image_hash: "ab".repeat(32),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol: 0,
        capabilities: vec!["attach".into()],
        integrity: Integrity::from_rid(Integrity::MEDIUM),
        windows_session: 1,
        in_job: false,
        desktop_job: false,
    }
}

fn no_argv(_: u32) -> Option<Vec<String>> {
    None
}

fn enabled() -> bool {
    true
}

fn new_host() -> (Arc<Host<FakeLauncher>>, FakeLauncher) {
    let launcher = FakeLauncher::default();
    (
        Host::new(launcher.clone(), 77, "bundled", no_argv, enabled),
        launcher,
    )
}

/// A served connection's client end, authenticated as `pid`.
fn raw_client(host: &Arc<Host<FakeLauncher>>, pid: u32) -> Arc<dyn Link> {
    let (client, server) = memory_pair();
    let host = host.clone();
    thread::spawn(move || serve_connection(&host, Arc::new(server), &identity(), || Ok(pid)));
    Arc::new(client)
}

fn connect_as(host: &Arc<Host<FakeLauncher>>, pid: u32, role: Role) -> Connection {
    Connection::hello(raw_client(host, pid), role).unwrap()
}

fn connect(host: &Arc<Host<FakeLauncher>>) -> Connection {
    connect_as(host, CLIENT_PID, Role::Cli)
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| word.to_string()).collect()
}

fn create(launch_id: &str, name: &str) -> Op {
    Op::CreateSession(CreateSession {
        launch_id: launch_id.into(),
        name: name.into(),
        cwd: WORK.into(),
        env: strings(&["Path=/bin", "COLORTERM=truecolor", "TERM=dumb"]),
        argv: strings(&[PROGRAM, "--run", "claude"]),
        size: Size { cols: 80, rows: 24 },
    })
}

fn pane(name: &str) -> Target {
    Target::Name(name.into())
}

fn capture(connection: &mut Connection, target: Target) -> String {
    connection
        .call::<Captured>(Op::Capture {
            target,
            styled: false,
        })
        .unwrap()
        .text
}

fn info(connection: &mut Connection, target: Target) -> PaneDetails {
    connection.call(Op::PaneInfo { target }).unwrap()
}

#[test]
fn sessions_are_created_once_per_launch_and_listed_with_every_field() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    assert_eq!(client.host().protocol, 1);
    assert_eq!(client.host().pid, 77);
    let created: Created = client.call(create("launch-a", "claude/p/a")).unwrap();
    assert_eq!(
        created,
        Created {
            session: "$0".into(),
            pane: "%0".into(),
            name: "claude/p/a".into(),
            pid: 1000,
            existing: false,
        }
    );
    // A retried create starts nothing.
    let again: Created = client.call(create("launch-a", "claude/p/a")).unwrap();
    assert_eq!((again.session.as_str(), again.existing), ("$0", true));
    assert_eq!(launcher.count(), 1);
    // Another launch may not take the name.
    let error = client
        .call::<Created>(create("launch-b", "claude/p/a"))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Exists);
    let second: Created = client.call(create("launch-c", "codex/p/b")).unwrap();
    assert_eq!(
        (second.session.as_str(), second.pane.as_str()),
        ("$1", "%1")
    );

    // The program got the client's environment with the session's own.
    let program = launcher.program(0);
    assert_eq!(program.argv, [PROGRAM, "--run", "claude"]);
    assert_eq!(program.cwd, WORK);
    let variable = |name: &str| {
        program
            .environment
            .iter()
            .find(|(key, _)| key == name)
            .and_then(|(_, value)| value.clone())
    };
    assert_eq!(variable("TERM").as_deref(), Some("xterm-256color"));
    assert_eq!(variable("COLORTERM"), None);
    assert_eq!(variable("HGS_HOST_SESSION").as_deref(), Some("$0"));
    assert_eq!(variable("HGS_HOST_PANE").as_deref(), Some("%0"));
    assert_eq!(variable("Path").as_deref(), Some("/bin"));

    client
        .call::<Done>(Op::SetEnvironment {
            target: pane("claude/p/a"),
            name: RUN_MARKER.into(),
            value: Some("run-1".into()),
        })
        .unwrap();
    let listed: Listed = client.call(Op::List).unwrap();
    assert_eq!(listed.sessions.len(), 2);
    assert_eq!(listed.sessions[0].id, "$0");
    assert_eq!(listed.sessions[0].name, "claude/p/a");
    assert_eq!(listed.sessions[0].attached, 0);
    assert_eq!(listed.sessions[0].launch_id, "launch-a");
    assert!(listed.sessions[0].created > 1_600_000_000);
    assert_eq!(listed.panes[0].session, "claude/p/a");
    assert_eq!(listed.panes[0].session_id, "$0");
    assert_eq!(listed.panes[0].pane, "%0");
    assert_eq!(listed.panes[0].pid, 1000);
    assert!(!listed.panes[0].dead);
    assert_eq!(listed.panes[0].run_marker, "run-1");
    assert_eq!(listed.panes[1].run_marker, "");
    assert_eq!(listed.panes[0].current_path, WORK);
    assert!(listed.clients.is_empty());

    for target in [
        pane("codex/p/b"),
        Target::Session("$1".into()),
        Target::Pane("%1".into()),
    ] {
        let resolved: Resolved = client.call(Op::ResolveTarget { target }).unwrap();
        assert_eq!(
            resolved,
            Resolved {
                session: "$1".into(),
                name: "codex/p/b".into(),
                pane: "%1".into()
            }
        );
    }
    let missing = client
        .call::<Resolved>(Op::ResolveTarget {
            target: Target::Pane("%9".into()),
        })
        .unwrap_err();
    assert_eq!(
        (missing.code, missing.message.as_str()),
        (ErrorCode::NotFound, "can't find pane: %9")
    );
}

/// A pane's current directory is the one its session started in (tmux's
/// `pane_current_path` for a program that never changes directory): the host
/// does not read another process's current directory, and without this an
/// untracked session would have no folder at all.
#[test]
fn panes_report_the_directory_their_session_started_in() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    client
        .call::<Created>(create("launch-a", "claude/p/a"))
        .unwrap();
    let Op::CreateSession(mut other) = create("launch-b", "pwsh/p/b") else {
        unreachable!()
    };
    other.cwd = OTHER_WORK.into();
    client
        .call::<Created>(Op::CreateSession(other.clone()))
        .unwrap();
    assert_eq!(launcher.program(1).cwd, OTHER_WORK);
    // A retried create reports its first session and moves nothing.
    other.cwd = WORK.into();
    let again: Created = client.call(Op::CreateSession(other)).unwrap();
    assert!(again.existing);
    // A rename keeps the directory.
    client
        .call::<Done>(Op::Rename {
            target: pane("pwsh/p/b"),
            name: "pwsh/p/c".into(),
        })
        .unwrap();

    let listed: Listed = client.call(Op::List).unwrap();
    let paths: Vec<_> = listed
        .panes
        .iter()
        .map(|pane| (pane.session.as_str(), pane.current_path.as_str()))
        .collect();
    assert_eq!(paths, [("claude/p/a", WORK), ("pwsh/p/c", OTHER_WORK)]);
    // On the wire it is `list`'s `current_path`, which the host backend
    // reports as the pane's path (`TerminalBackend::pane_paths`).
    let wire: serde_json::Value = client.call(Op::List).unwrap();
    assert_eq!(wire["panes"][1]["current_path"], OTHER_WORK);
}

/// Whether the host of `sessions_start_only_while_local_sessions_are_enabled`
/// finds local sessions enabled.
static ENABLED: AtomicBool = AtomicBool::new(true);

#[test]
fn sessions_start_only_while_local_sessions_are_enabled() {
    let launcher = FakeLauncher::default();
    let host = Host::new(launcher.clone(), 77, "bundled", no_argv, || {
        ENABLED.load(Ordering::SeqCst)
    });
    let mut client = connect(&host);
    let created: Created = client.call(create("launch-a", "claude/p/a")).unwrap();
    // `hgs session-host uninstall` removed the marker: nothing new starts.
    ENABLED.store(false, Ordering::SeqCst);
    let error = client
        .call::<Created>(create("launch-b", "claude/p/b"))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Refused);
    assert!(
        error.message.contains("hgs session-host install"),
        "{}",
        error.message
    );
    assert_eq!(launcher.count(), 1);
    // A retried create still finds what its first request started.
    let again: Created = client.call(create("launch-a", "claude/p/a")).unwrap();
    assert_eq!((again.session, again.existing), (created.session, true));
    let listed: Listed = client.call(Op::List).unwrap();
    assert_eq!(listed.sessions.len(), 1);
    ENABLED.store(true, Ordering::SeqCst);
    let second: Created = client.call(create("launch-b", "claude/p/b")).unwrap();
    assert_eq!((second.session.as_str(), second.existing), ("$1", false));
    assert_eq!(launcher.count(), 2);
}
#[test]
fn creates_check_their_request_and_report_a_failed_start() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    let mut cases = Vec::new();
    for change in 0..5 {
        let Op::CreateSession(mut request) = create("launch", "claude/p/a") else {
            unreachable!()
        };
        match change {
            0 => request.launch_id.clear(),
            1 => request.name = "bad\nname".into(),
            2 => request.argv = strings(&["hgs", "--run"]),
            3 => request.cwd = "relative".into(),
            _ => request.size = Size { cols: 0, rows: 24 },
        }
        cases.push(Op::CreateSession(request));
    }
    for op in cases {
        assert_eq!(
            client.call::<Created>(op).unwrap_err().code,
            ErrorCode::Invalid
        );
    }
    launcher.0.fail.store(true, Ordering::SeqCst);
    let error = client
        .call::<Created>(create("launch", "claude/p/a"))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal);
    assert!(
        error.message.contains("no such program"),
        "{}",
        error.message
    );
    // The launch ID was not used up.
    let created: Created = client.call(create("launch", "claude/p/a")).unwrap();
    assert!(!created.existing);
}

#[test]
fn screens_keys_and_pastes_reach_the_pane_as_tmux_sends_them() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    client.call::<Created>(create("l", "claude/p/a")).unwrap();
    let program = launcher.program(0);
    program.write(b"hello\r\nworld \x1b[31mred\x1b[0m");
    eventually("the screen", || {
        capture(&mut client, pane("claude/p/a")).starts_with("hello\nworld red\n")
    });
    let styled: Captured = client
        .call(Op::Capture {
            target: pane("claude/p/a"),
            styled: true,
        })
        .unwrap();
    assert!(
        styled
            .text
            .starts_with("hello\nworld \x1b[31mred\x1b[39m\n"),
        "{:?}",
        styled.text
    );
    let details = info(&mut client, pane("claude/p/a"));
    assert_eq!(
        (
            details.width,
            details.height,
            details.cursor_x,
            details.cursor_y
        ),
        (80, 24, 9, 1)
    );
    assert_eq!(details.server_pid, 77);
    assert_eq!(details.pane_pid, 1000);
    assert_eq!(details.current_command, "fake");
    assert_eq!(details.tty, "conpty:%0");
    assert!(!details.in_mode && !details.dead && !details.bracketed_paste);

    client
        .call::<Done>(Op::SendKeys {
            target: pane("claude/p/a"),
            keys: vec![
                Key::Literal("echo marker".into()),
                Key::Named("Enter".into()),
            ],
        })
        .unwrap();
    assert_eq!(program.input_until(b"\r"), b"echo marker\r");
    // Cursor keys follow the application's DECCKM.
    program.write(b"\x1b[?1h");
    eventually("DECCKM", || {
        client
            .call::<Done>(Op::SendKeys {
                target: pane("claude/p/a"),
                keys: vec![Key::Named("Up".into())],
            })
            .unwrap();
        program.read_input(Duration::from_millis(50)) == b"\x1bOA"
    });
    // An empty list only checks the pane.
    client
        .call::<Done>(Op::SendKeys {
            target: pane("claude/p/a"),
            keys: Vec::new(),
        })
        .unwrap();

    // Pastes are bracketed only when asked and the application enabled it.
    let paste = |client: &mut Connection, bracketed| {
        client
            .call::<Done>(Op::Paste {
                target: pane("claude/p/a"),
                text: "a\nb".into(),
                bracketed,
            })
            .unwrap();
    };
    paste(&mut client, true);
    assert_eq!(program.input_until(b"b"), b"a\nb");
    program.write(b"\x1b[?2004h");
    eventually("bracketed paste", || {
        info(&mut client, pane("claude/p/a")).bracketed_paste
    });
    paste(&mut client, true);
    assert_eq!(program.input_until(b"\x1b[201~"), b"\x1b[200~a\nb\x1b[201~");
    paste(&mut client, false);
    assert_eq!(program.input_until(b"b"), b"a\nb");
    assert_eq!(paste_bytes("", true), b"\x1b[200~\x1b[201~");
    // A pasted end marker cannot end the paste early.
    client
        .call::<Done>(Op::Paste {
            target: pane("claude/p/a"),
            text: "x\x1b[201~\ry".into(),
            bracketed: true,
        })
        .unwrap();
    assert_eq!(
        program.input_until(b"\x1b[201~"),
        b"\x1b[200~x^[[201~\ry\x1b[201~"
    );
}

#[test]
fn pastes_show_control_characters_as_tmux_does() {
    // Recorded from tmux 3.8 `paste-buffer -p -r` with bracketed paste on:
    // every ASCII byte, then characters it leaves alone (C1 controls too).
    let ascii: String = (0u8..0x80).map(char::from).collect();
    let mut expected =
        b"\x1b[200~^@^A^B^C^D^E^F\x07\x08\t\n^K^L\r^N^O^P^Q^R^S^T^U^V^W^X^Y^Z^[^\\^]^^^_".to_vec();
    expected.extend((0x20u8..0x7f).collect::<Vec<_>>());
    expected.extend_from_slice(b"^?\x1b[201~");
    assert_eq!(paste_bytes(&ascii, true), expected);
    let other = "e\u{301}\u{85}\u{9b}\u{a0}\u{ad}\u{200b}\u{feff}\u{fffe}😀";
    assert_eq!(paste_bytes(other, false), other.as_bytes());
}

#[test]
fn queries_get_one_answer_from_the_host_and_never_reach_clients() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    client.call::<Created>(create("l", "claude/p/a")).unwrap();
    let program = launcher.program(0);
    let (attached, stream) = connect_as(&host, 9, Role::Attach)
        .attach(Attach {
            target: pane("claude/p/a"),
            size: Size { cols: 80, rows: 24 },
            run_id: None,
        })
        .unwrap();
    assert_eq!(attached.client, "c1");
    // The repaint comes first.
    let Event::Output(repaint) = stream.next().unwrap().unwrap() else {
        panic!("no repaint")
    };
    assert!(repaint.starts_with(b"\x1b[?25l"), "{repaint:?}");

    program.write(b"ab\x1b[6n\x1b[c\x1b]11;?\x07cd");
    // The model answers CPR and DA1 once each; OSC 11 stays unanswered, as in tmux.
    assert_eq!(program.input_until(b"c"), b"\x1b[1;3R\x1b[?1;2c");
    let mut relayed = Vec::new();
    eventually("relayed output", || {
        if let Ok(Some(Event::Output(bytes))) = stream.next() {
            relayed.extend(bytes);
        }
        relayed.ends_with(b"cd")
    });
    assert_eq!(relayed, b"abcd");

    // The client's terminal answers too; only its typing reaches the pane.
    stream
        .send(b"\x1b[?1;2c\x1b[5;1Rtyped\x1b]11;rgb:0/0/0\x07")
        .unwrap();
    assert_eq!(program.input_until(b"typed"), b"typed");
    // A lone ESC goes out once nothing completes it.
    stream.send(b"\x1b").unwrap();
    assert_eq!(program.input_until(b"\x1b"), b"\x1b");
    assert!(program.read_input(session::ESCAPE_TIME * 3).is_empty());
}

#[test]
fn only_an_unviewed_pane_grows_and_the_next_client_sets_its_size_again() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    client.call::<Created>(create("l", "claude/p/a")).unwrap();
    let program = launcher.program(0);
    let mut grow = |rows| {
        client
            .call::<Grown>(Op::GrowUnviewed {
                target: pane("claude/p/a"),
                rows,
            })
            .map(|grown| grown.grown)
    };
    assert_eq!(grow(48), Ok(true));
    assert_eq!(program.sizes().last(), Some(&Size { cols: 80, rows: 48 }));
    // It never shrinks, nor resizes to the size it has.
    assert_eq!(grow(48), Ok(false));
    assert_eq!(grow(30), Ok(false));
    assert_eq!(grow(1025).unwrap_err().code, ErrorCode::Invalid);
    let details = info(&mut client, pane("claude/p/a"));
    assert_eq!((details.width, details.height), (80, 48));

    // A viewed pane keeps its client's size.
    let small = Size { cols: 60, rows: 20 };
    let (attached, stream) = connect_as(&host, 10, Role::Attach)
        .attach(Attach {
            target: pane("claude/p/a"),
            size: small,
            run_id: None,
        })
        .unwrap();
    assert_eq!(program.sizes().last(), Some(&small));
    let resizes = program.sizes().len();
    let mut client = connect(&host);
    let mut grow = |rows| {
        client
            .call::<Grown>(Op::GrowUnviewed {
                target: pane("claude/p/a"),
                rows,
            })
            .map(|grown| grown.grown)
    };
    assert_eq!(grow(96), Ok(false));
    assert_eq!(program.sizes().len(), resizes);
    drop(stream);
    drop(attached);
    eventually("the detach", || {
        let listed: Listed = connect(&host).call(Op::List).unwrap();
        listed.sessions[0].attached == 0
    });
    // Detached, it grows from the size the client left.
    assert_eq!(grow(40), Ok(true));
    assert_eq!(program.sizes().last(), Some(&Size { cols: 60, rows: 40 }));
    program.exit(0);
    eventually("the end", || grow(96).is_err());
}

#[test]
fn attached_clients_follow_the_latest_size_and_learn_of_the_end() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    client.call::<Created>(create("l", "claude/p/a")).unwrap();
    let program = launcher.program(0);
    program.write(b"screen text");
    eventually("the screen", || {
        capture(&mut client, pane("claude/p/a")).starts_with("screen text")
    });
    let attach = |host: &Arc<Host<FakeLauncher>>, pid, size| {
        connect_as(host, pid, Role::Attach)
            .attach(Attach {
                target: pane("claude/p/a"),
                size,
                run_id: None,
            })
            .unwrap()
    };
    let small = Size { cols: 60, rows: 20 };
    let large = Size {
        cols: 120,
        rows: 40,
    };
    let (first, one) = attach(&host, 10, small);
    let Event::Output(repaint) = one.next().unwrap().unwrap() else {
        panic!("no repaint")
    };
    assert!(String::from_utf8_lossy(&repaint).contains("screen text"));
    let details = info(&mut client, pane("claude/p/a"));
    assert_eq!((details.width, details.height), (60, 20));
    let (second, two) = attach(&host, 11, large);
    assert_eq!(program.sizes().last(), Some(&large));
    // Typing makes a client the latest.
    one.send(b"x").unwrap();
    program.input_until(b"x");
    eventually("the resize", || program.sizes().last() == Some(&small));
    one.resize(Size { cols: 70, rows: 21 }).unwrap();
    eventually("the resize", || {
        info(&mut client, pane("claude/p/a")).width == 70
    });

    let listed: Listed = client.call(Op::List).unwrap();
    assert_eq!(listed.sessions[0].attached, 2);
    let ids: Vec<(String, u32)> = listed
        .clients
        .iter()
        .map(|entry| (entry.id.clone(), entry.pid))
        .collect();
    assert_eq!(
        ids,
        [(second.client.clone(), 11), (first.client.clone(), 10)]
    );
    client
        .call::<Done>(Op::Notify {
            client: second.client.clone(),
            text: "Pasted".into(),
        })
        .unwrap();
    let notice = loop {
        match two.next().unwrap().unwrap() {
            Event::Control(control) => break control,
            Event::Output(_) => {}
        }
    };
    assert_eq!(
        notice,
        Control::Notice {
            text: "Pasted".into()
        }
    );
    assert_eq!(
        client
            .call::<Done>(Op::Notify {
                client: "c99".into(),
                text: "x".into()
            })
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );

    // The first client detaches: the pane takes the other's size.
    one.detach().unwrap();
    while one.next().unwrap().is_some() {}
    eventually("the detach", || {
        info(&mut client, pane("claude/p/a")).width == 120
    });
    let clients: super::protocol::ClientList = client
        .call(Op::ListClients {
            target: Some(pane("claude/p/a")),
        })
        .unwrap();
    assert_eq!(clients.clients.len(), 1);

    // The session ends: the client gets its exit code, then the end.
    program.exit(3);
    let mut events = Vec::new();
    while let Some(event) = two.next().unwrap() {
        events.push(event);
    }
    assert_eq!(
        events.last(),
        Some(&Event::Control(Control::Exit { code: Some(3) }))
    );
    eventually("the end", || {
        client.call::<Listed>(Op::List).unwrap().sessions.is_empty()
    });
    assert!(program.hung_up.load(Ordering::SeqCst) && program.ended.load(Ordering::SeqCst));
    // A retried create of the ended launch still finds it.
    let created: Created = client.call(create("l", "claude/p/a")).unwrap();
    assert_eq!((created.session.as_str(), created.existing), ("$0", true));
}

#[test]
fn attach_checks_the_run_and_the_target() {
    let (host, _launcher) = new_host();
    let mut client = connect(&host);
    client.call::<Created>(create("l", "claude/p/a")).unwrap();
    let request = |run_id: Option<&str>| Attach {
        target: pane("claude/p/a"),
        size: Size { cols: 80, rows: 24 },
        run_id: run_id.map(String::from),
    };
    let refused = connect(&host).attach(request(Some("run-1"))).err().unwrap();
    assert_eq!(refused.code, ErrorCode::Refused);
    client
        .call::<Done>(Op::SetEnvironment {
            target: pane("claude/p/a"),
            name: RUN_MARKER.into(),
            value: Some("run-1".into()),
        })
        .unwrap();
    let (attached, _stream) = connect(&host).attach(request(Some("run-1"))).unwrap();
    assert_eq!(
        attached,
        Attached {
            client: "c1".into(),
            session: "$0".into(),
            name: "claude/p/a".into(),
            pane: "%0".into()
        }
    );
    let missing = connect(&host)
        .attach(Attach {
            target: pane("nothing"),
            ..request(None)
        })
        .err()
        .unwrap();
    assert_eq!(missing.code, ErrorCode::NotFound);
}

#[test]
fn variables_names_and_kills() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    client.call::<Created>(create("l", "claude/p/a")).unwrap();
    client.call::<Created>(create("m", "claude/p/b")).unwrap();
    let show = |client: &mut Connection, name: &str| {
        client
            .call::<EnvValue>(Op::ShowEnvironment {
                target: Target::Session("$0".into()),
                name: name.into(),
            })
            .unwrap()
            .value
    };
    assert_eq!(show(&mut client, "HGS_CLIENT"), None);
    let set = |client: &mut Connection, value: Option<&str>| {
        client
            .call::<Done>(Op::SetEnvironment {
                target: Target::Session("$0".into()),
                name: "HGS_CLIENT".into(),
                value: value.map(String::from),
            })
            .unwrap()
    };
    set(&mut client, Some("phone"));
    assert_eq!(show(&mut client, "HGS_CLIENT").as_deref(), Some("phone"));
    set(&mut client, None);
    assert_eq!(show(&mut client, "HGS_CLIENT"), None);

    let rename = |client: &mut Connection, name: &str| {
        client.call::<Done>(Op::Rename {
            target: Target::Session("$0".into()),
            name: name.into(),
        })
    };
    assert_eq!(
        rename(&mut client, "claude/p/b").unwrap_err().code,
        ErrorCode::Exists
    );
    rename(&mut client, "claude/p/c").unwrap();
    assert_eq!(info(&mut client, pane("claude/p/c")).session_id, "$0");

    // A kill hangs the console up; the program's close handler ends it.
    client
        .call::<Done>(Op::Kill {
            target: pane("claude/p/c"),
        })
        .unwrap();
    eventually("the kill", || {
        client.call::<Listed>(Op::List).unwrap().sessions.len() == 1
    });
    let program = launcher.program(0);
    assert!(program.hung_up.load(Ordering::SeqCst));
    assert_eq!(*lock(&program.exit), Some(0xC000_013A));
    // Keys to an ended session fail.
    assert_eq!(
        client
            .call::<Done>(Op::SendKeys {
                target: pane("claude/p/c"),
                keys: vec![Key::Named("Enter".into())],
            })
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

/// Task #14 (P2.4): `list` carries a structure counter that moves when a
/// session starts, is renamed or ends, and a per-pane output counter that
/// moves on every output of the pane and nothing else.
#[test]
fn list_counters_move_on_output_structure_and_exit() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    let list = |client: &mut Connection| client.call::<Listed>(Op::List).unwrap();
    let empty = list(&mut client).structure.expect("a current host reports the structure");
    client.call::<Created>(create("l", "claude/p/a")).unwrap();
    client.call::<Created>(create("m", "claude/p/b")).unwrap();
    let created = list(&mut client);
    let structure = created.structure.unwrap();
    assert!(structure > empty, "a new session moves the structure");
    let output = |listed: &Listed, name: &str| listed.panes.iter().find(|pane| pane.session == name).and_then(|pane| pane.output);
    let (a, b) = (output(&created, "claude/p/a").unwrap(), output(&created, "claude/p/b").unwrap());

    // Output moves its own pane's counter, not the other's, nor the structure.
    launcher.program(0).write(b"Do you trust this folder? 1. Yes 2. No\r\n");
    eventually("the output counter", || output(&list(&mut client), "claude/p/a").unwrap() > a);
    let written = list(&mut client);
    assert_eq!(output(&written, "claude/p/b"), Some(b));
    assert_eq!(written.structure, Some(structure));
    // A variable is no structure change and no output.
    client
        .call::<Done>(Op::SetEnvironment { target: pane("claude/p/b"), name: RUN_MARKER.into(), value: Some("run-b".into()) })
        .unwrap();
    let marked = list(&mut client);
    assert_eq!((marked.structure, output(&marked, "claude/p/b")), (Some(structure), Some(b)));

    // A rename moves the structure.
    client.call::<Done>(Op::Rename { target: Target::Session("$1".into()), name: "claude/p/c".into() }).unwrap();
    let renamed = list(&mut client).structure.unwrap();
    assert!(renamed > structure);

    // The exit of its process: the pane is listed dead with a moved counter
    // or has left the listing, which moves the structure.
    let before = output(&list(&mut client), "claude/p/a").unwrap();
    launcher.program(0).exit(0);
    eventually("the exit", || {
        let listed = list(&mut client);
        match listed.panes.iter().find(|pane| pane.session == "claude/p/a") {
            Some(pane) => pane.dead && pane.output.unwrap() > before,
            None => listed.structure.unwrap() > renamed,
        }
    });
    eventually("the end", || {
        let listed = list(&mut client);
        listed.sessions.len() == 1 && listed.structure.unwrap() > renamed
    });
}

/// A supervisor on its own connection, answering with `evidence`.
fn supervise(
    host: &Arc<Host<FakeLauncher>>,
    pid: u32,
    evidence: ConsoleEvidence,
    program: Arc<Program>,
) -> Result<thread::JoinHandle<Vec<SupervisorOp>>, ErrorBody> {
    let link =
        connect_as(host, pid, Role::Supervisor).register_supervisor(SupervisorRegistration {
            session: "$0".into(),
            pane: "%0".into(),
            pid,
        })?;
    Ok(thread::spawn(move || {
        let mut seen = Vec::new();
        let _ = answer_requests(&*link, |op| {
            seen.push(op.clone());
            match op {
                SupervisorOp::InputState => Ok(serde_json::to_value(&evidence).unwrap()),
                SupervisorOp::Stop => {
                    // Ctrl+Break ends the agent; the supervisor records and exits.
                    program.exit(0);
                    Ok(serde_json::json!({}))
                }
                SupervisorOp::Unsupported => Err(ErrorBody::new(ErrorCode::Unsupported, "?")),
            }
        });
        seen
    }))
}

fn evidence(echo: bool) -> ConsoleEvidence {
    ConsoleEvidence {
        line_input: false,
        echo_input: echo,
        processed_input: false,
        virtual_terminal_input: Some(true),
        processes: vec![
            ConsoleProcess {
                pid: 1000,
                name: "hgs.exe".into(),
            },
            ConsoleProcess {
                pid: 2000,
                name: "node.exe".into(),
            },
        ],
        agent_pid: Some(2000),
        agent_alive: true,
        agent_in_job: true,
    }
}

#[test]
fn the_supervisor_channel_carries_input_checks_and_stops() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    client.call::<Created>(create("l", "claude/p/a")).unwrap();
    let program = launcher.program(0);
    let claim = |pid| {
        Op::InputState(InputClaim {
            target: Target::Pane("%0".into()),
            name: "claude/p/a".into(),
            run_id: "run-1".into(),
            agent: "claude".into(),
            pid: Some(pid),
        })
    };
    // Without the run marker and bracketed paste the pane is not ready.
    let error = client.call::<InputReady>(claim(2000)).unwrap_err();
    assert_eq!(
        (error.code, error.message.as_str()),
        (ErrorCode::Refused, NOT_AGENT_INPUT)
    );
    client
        .call::<Done>(Op::SetEnvironment {
            target: pane("claude/p/a"),
            name: RUN_MARKER.into(),
            value: Some("run-1".into()),
        })
        .unwrap();
    program.write(b"\x1b[?2004h> ");
    eventually("bracketed paste", || {
        info(&mut client, pane("claude/p/a")).bracketed_paste
    });
    // No supervisor yet: signals and input checks fail closed.
    let no_supervisor = client.call::<InputReady>(claim(2000)).unwrap_err();
    assert_eq!(no_supervisor.code, ErrorCode::Refused);
    assert!(
        no_supervisor.message.contains("no supervisor"),
        "{}",
        no_supervisor.message
    );
    let pause = |client: &mut Connection, signal| {
        client.call::<Done>(Op::Signal {
            target: pane("claude/p/a"),
            signal,
        })
    };
    assert_eq!(
        pause(&mut client, Signal::Pause).unwrap_err().code,
        ErrorCode::Refused
    );

    // Only the session's own processes may register, under their own pid.
    let stranger = supervise(&host, 3000, evidence(false), program.clone())
        .err()
        .unwrap();
    assert_eq!(stranger.code, ErrorCode::Refused);
    lock(&program.members).push(1500);
    let lying = connect_as(&host, 1500, Role::Supervisor)
        .register_supervisor(SupervisorRegistration {
            session: "$0".into(),
            pane: "%0".into(),
            pid: 1000,
        })
        .err()
        .unwrap();
    assert_eq!(lying.code, ErrorCode::Refused);
    let supervisor = supervise(&host, 1000, evidence(false), program.clone()).unwrap();
    let twice = supervise(&host, 1500, evidence(false), program.clone())
        .err()
        .unwrap();
    assert_eq!(twice.code, ErrorCode::Exists);

    let ready: InputReady = client.call(claim(2000)).unwrap();
    assert_eq!((ready.cursor_x, ready.cursor_y), (2, 0));
    let error = client.call::<InputReady>(claim(2001)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Refused);

    // A pause is the supervisor's stop; the session then ends.
    pause(&mut client, Signal::Pause).unwrap();
    eventually("the end", || {
        client.call::<Listed>(Op::List).unwrap().sessions.is_empty()
    });
    assert_eq!(
        supervisor.join().unwrap(),
        [
            SupervisorOp::InputState,
            SupervisorOp::InputState,
            SupervisorOp::Stop
        ]
    );
}

#[test]
fn sent_ctrl_letters_reach_a_program_that_reads_key_records_as_records() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    client.call::<Created>(create("l", "codex/p/a")).unwrap();
    let program = launcher.program(0);
    let send = |client: &mut Connection, keys: &[&str]| {
        let keys = keys
            .iter()
            .map(|key| match key.strip_prefix("literal:") {
                Some(text) => Key::Literal(text.into()),
                None => Key::Named(key.to_string()),
            })
            .collect();
        client
            .call::<Done>(Op::SendKeys {
                target: pane("codex/p/a"),
                keys,
            })
            .unwrap();
    };
    let ctrl_c: &[u8] = b"\x1b[67;46;99;1;8;1_\x1b[67;46;99;0;8;1_";
    let ctrl_u: &[u8] = b"\x1b[85;22;117;1;8;1_\x1b[85;22;117;0;8;1_";
    // Until the console asks for win32-input-mode, keys are plain VT input.
    send(&mut client, &["C-c", "C-u", "Enter"]);
    assert_eq!(program.input_until(b"\r"), b"\x03\x15\r");
    // ConPTY asks at its start; the session has no supervisor yet, as an
    // untracked one, so the bytes go as before.
    program.write(b"\x1b[?9001h\x1b[?1004hready");
    eventually("the request", || {
        capture(&mut client, pane("codex/p/a")).starts_with("ready")
    });
    send(&mut client, &["C-c", "Enter"]);
    assert_eq!(program.input_until(b"\r"), b"\x03\r");

    // A supervisor that reports its console's VT input flag as `vt_input`
    // holds, and counts the requests.
    let vt_input = Arc::new(Mutex::new(Some(true)));
    let asked = Arc::new(AtomicU32::new(0));
    let link = connect_as(&host, program.pid, Role::Supervisor)
        .register_supervisor(SupervisorRegistration {
            session: "$0".into(),
            pane: "%0".into(),
            pid: program.pid,
        })
        .unwrap();
    {
        let (vt_input, asked) = (vt_input.clone(), asked.clone());
        thread::spawn(move || {
            answer_requests(&*link, |op| {
                assert_eq!(op, SupervisorOp::InputState);
                asked.fetch_add(1, Ordering::SeqCst);
                let reported = ConsoleEvidence {
                    virtual_terminal_input: *lock(&vt_input),
                    ..evidence(false)
                };
                Ok(serde_json::to_value(reported).unwrap())
            })
        });
    }
    // Claude Code, Kimi and the fake agent read VT input (0x0208): ConPTY
    // would give them a key record's letter alone, so the byte goes as before.
    send(&mut client, &["C-c", "Enter"]);
    assert_eq!(program.input_until(b"\r"), b"\x03\r");
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    // Codex reads key records (0x0098): Ctrl+letter goes as records, and the
    // supervisor is asked once for the whole send.
    *lock(&vt_input) = Some(false);
    send(
        &mut client,
        &[
            "C-c",
            "C-u",
            "C-M-a",
            "C-h",
            "Tab",
            "literal:x\x03",
            "Enter",
        ],
    );
    assert_eq!(
        program.input_until(b"\r"),
        [ctrl_c, ctrl_u, b"\x1b\x01\x08\tx\x03\r"].concat()
    );
    assert_eq!(asked.load(Ordering::SeqCst), 2);
    // A send without Ctrl+letter asks nothing.
    send(&mut client, &["literal:hi", "Escape", "Up", "Enter"]);
    assert_eq!(program.input_until(b"\r"), b"hi\x1b\x1b[A\r");
    assert_eq!(asked.load(Ordering::SeqCst), 2);
    // Pastes, query answers and what an attached client types stay as they
    // are: a client's terminal sends what it sends to any program.
    client
        .call::<Done>(Op::Paste {
            target: pane("codex/p/a"),
            text: "a\x03b".into(),
            bracketed: false,
        })
        .unwrap();
    assert_eq!(program.input_until(b"b"), b"a^Cb");
    program.write(b"\x1b[6n");
    assert_eq!(program.input_until(b"R"), b"\x1b[1;6R");
    let (_attached, stream) = connect_as(&host, 9, Role::Attach)
        .attach(Attach {
            target: pane("codex/p/a"),
            size: Size { cols: 80, rows: 24 },
            run_id: None,
        })
        .unwrap();
    stream.send(b"\x03\x15x").unwrap();
    assert_eq!(program.input_until(b"x"), b"\x03\x15x");
    assert_eq!(asked.load(Ordering::SeqCst), 2);
    // A supervisor that does not report the flag (an older one).
    *lock(&vt_input) = None;
    send(&mut client, &["C-u", "Enter"]);
    assert_eq!(program.input_until(b"\r"), b"\x15\r");
    assert_eq!(asked.load(Ordering::SeqCst), 3);

    // Once the console resets the mode, keys are plain VT input again and
    // the supervisor is not asked.
    *lock(&vt_input) = Some(false);
    program.write(b"\x1b[?9001ldone");
    eventually("the reset", || {
        capture(&mut client, pane("codex/p/a")).starts_with("readydone")
    });
    send(&mut client, &["C-c", "Enter"]);
    assert_eq!(program.input_until(b"\r"), b"\x03\r");
    assert_eq!(asked.load(Ordering::SeqCst), 3);
    // The client's terminal never saw the mode.
    let mut relayed = Vec::new();
    eventually("relayed output", || {
        if let Ok(Some(Event::Output(bytes))) = stream.next() {
            relayed.extend(bytes);
        }
        relayed.ends_with(b"done")
    });
    let relayed = String::from_utf8_lossy(&relayed);
    assert!(!relayed.contains("9001"), "{relayed:?}");
}

#[test]
fn raw_mode_evidence_and_terminate_go_through_the_supervisor() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    client.call::<Created>(create("l", "claude/p/a")).unwrap();
    let program = launcher.program(0);
    client
        .call::<Done>(Op::SetEnvironment {
            target: pane("claude/p/a"),
            name: RUN_MARKER.into(),
            value: Some("run-1".into()),
        })
        .unwrap();
    program.write(b"\x1b[?2004h");
    eventually("bracketed paste", || {
        info(&mut client, pane("claude/p/a")).bracketed_paste
    });
    let supervisor = supervise(&host, 1000, evidence(true), program.clone()).unwrap();
    let error = client
        .call::<InputReady>(Op::InputState(InputClaim {
            target: pane("claude/p/a"),
            name: "claude/p/a".into(),
            run_id: "run-1".into(),
            agent: "claude".into(),
            pid: Some(2000),
        }))
        .unwrap_err();
    assert_eq!(error.message, NOT_RAW);
    // Terminate answers at once; the stop ends the session within its grace.
    client
        .call::<Done>(Op::Signal {
            target: pane("claude/p/a"),
            signal: Signal::Terminate,
        })
        .unwrap();
    eventually("the end", || {
        client.call::<Listed>(Op::List).unwrap().sessions.is_empty()
    });
    assert_eq!(
        supervisor.join().unwrap(),
        [SupervisorOp::InputState, SupervisorOp::Stop]
    );
}

#[test]
fn a_just_answered_registration_is_awaited_and_a_closed_one_is_not() {
    // A client may act on an answered registration before the host's
    // connection thread has opened the channel (seen on a hosted runner).
    let (link, _peer) = memory_pair();
    let channel = SupervisorChannel::new(Arc::new(link));
    assert!(!channel.is_open());
    let opener = {
        let channel = channel.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            channel.open();
        })
    };
    assert!(channel.wait_open(LIMIT));
    opener.join().unwrap();
    assert!(channel.is_open());

    let (link, _peer) = memory_pair();
    let closed = SupervisorChannel::new(Arc::new(link));
    closed.close();
    let started = Instant::now();
    assert!(!closed.wait_open(LIMIT));
    assert!(started.elapsed() < Duration::from_secs(1));

    let (link, _peer) = memory_pair();
    let never = SupervisorChannel::new(Arc::new(link));
    assert!(!never.wait_open(Duration::from_millis(50)));
}

#[test]
fn hellos_are_checked_and_bad_requests_answered() {
    let (host, _launcher) = new_host();
    // Authentication refuses the client.
    let (client, server) = memory_pair();
    let served = {
        let host = host.clone();
        thread::spawn(move || {
            serve_connection(&host, Arc::new(server), &identity(), || {
                Err(ErrorBody::new(
                    ErrorCode::Refused,
                    "the session host runs elevated",
                ))
            })
        })
    };
    let refused = Connection::hello(Arc::new(client), Role::Cli)
        .err()
        .unwrap();
    assert!(matches!(refused, ClientError::Refused(ref body) if body.message.contains("elevated")));
    served.join().unwrap();

    // No common protocol.
    let link = raw_client(&host, 1);
    let mut hello = Hello::new(Role::Cli);
    hello.protocols = Protocols { min: 7, max: 9 };
    write_json(&*link, Kind::Json, &hello).unwrap();
    let answer: HelloAnswer = link.read_frame().unwrap().unwrap().json().unwrap();
    assert!(
        matches!(answer, HelloAnswer::Refused(ref body) if body.code == ErrorCode::Unsupported)
    );

    // A first frame that is no hello.
    let link = raw_client(&host, 1);
    link.write_frame(Kind::Stream, b"x").unwrap();
    let answer: HelloAnswer = link.read_frame().unwrap().unwrap().json().unwrap();
    assert!(matches!(answer, HelloAnswer::Refused(ref body) if body.code == ErrorCode::Invalid));

    // Bad and unknown requests are answered; the connection goes on.
    let link = raw_client(&host, 1);
    let mut connection = Connection::hello(link.clone(), Role::Cli).unwrap();
    link.write_frame(Kind::Json, br#"{"id":5,"op":"capture"}"#)
        .unwrap();
    let response: super::protocol::Response = link.read_frame().unwrap().unwrap().json().unwrap();
    assert_eq!(response.id, 5);
    assert_eq!(response.result.unwrap_err().code, ErrorCode::Invalid);
    assert_eq!(
        connection.request(Op::Unsupported).unwrap_err().code,
        ErrorCode::Unsupported
    );
    let status: super::protocol::HostStatus = connection.call(Op::HostStatus).unwrap();
    assert_eq!((status.sessions, status.conpty.as_str()), (0, "bundled"));
}

#[test]
fn an_idle_host_lingers_then_refuses_new_sessions() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    client.call::<Created>(create("l", "claude/p/a")).unwrap();
    // A stop is refused while sessions run.
    assert_eq!(
        client.call::<Done>(Op::StopHost).unwrap_err().code,
        ErrorCode::Busy
    );
    let waiting = {
        let host = host.clone();
        thread::spawn(move || {
            let started = Instant::now();
            host.wait_for_exit(Duration::from_millis(200));
            started.elapsed()
        })
    };
    thread::sleep(Duration::from_millis(400));
    assert!(!waiting.is_finished(), "a session runs");
    let ended = Instant::now();
    launcher.program(0).exit(0);
    let waited = waiting.join().unwrap();
    // It lingered after the last session ended.
    assert!(ended.elapsed() >= Duration::from_millis(200), "{waited:?}");
    assert!(host.exiting());
    // A create that races the decision is refused; its client starts a new host.
    let error = client
        .call::<Created>(create("m", "claude/p/b"))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Busy);

    // stop_host on an idle host ends the linger at once.
    let (host, _launcher) = new_host();
    let mut client = connect(&host);
    client.call::<Done>(Op::StopHost).unwrap();
    let started = Instant::now();
    host.wait_for_exit(Duration::from_secs(60));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn linger_counts_from_the_first_idle_moment() {
    let mut linger = Linger::new(Duration::from_secs(5));
    let start = Instant::now();
    assert_eq!(linger.next(false, start), Next::Serve);
    assert_eq!(
        linger.next(true, start),
        Next::Linger(Duration::from_secs(5))
    );
    assert_eq!(
        linger.next(true, start + Duration::from_secs(2)),
        Next::Linger(Duration::from_secs(3))
    );
    // Activity starts the count again.
    assert_eq!(
        linger.next(false, start + Duration::from_secs(3)),
        Next::Serve
    );
    assert_eq!(
        linger.next(true, start + Duration::from_secs(4)),
        Next::Linger(Duration::from_secs(5))
    );
    assert_eq!(
        linger.next(true, start + Duration::from_secs(9)),
        Next::Exit
    );
}

#[test]
fn foreground_is_the_newest_child_of_the_root() {
    assert_eq!(foreground(10, &[]), 10);
    let processes = [(20, 10, 5), (21, 10, 9), (22, 21, 12), (23, 99, 30)];
    assert_eq!(foreground(10, &processes), 21);
}

/// WhiteWh fork (owner, 2026-11-17): a client that attaches gets the lines
/// above the screen first, so its terminal can scroll back to them; a slow
/// client's repaint stays the screen alone.
#[test]
fn an_attaching_client_gets_the_history_before_the_screen() {
    let (host, launcher) = new_host();
    let mut connection = connect(&host);
    connection
        .call::<Created>(create("l", "claude/p/a"))
        .unwrap();
    let session = host.find(&pane("claude/p/a")).unwrap();
    let program = launcher.program(0);
    let mut printed = Vec::new();
    for n in 1..=60 {
        printed.extend(format!("answer line {n}\r\n").into_bytes());
    }
    printed.extend_from_slice(b"READY");
    program.write(&printed);
    eventually("the output", || {
        capture(&mut connection, pane("claude/p/a")).contains("READY")
    });
    let client = Attachment::new(1, 1, Size { cols: 80, rows: 24 });
    session.attach(client.clone()).unwrap();
    session.detach(client.id);
    let Some(Out::Stream(first)) = client.next() else {
        panic!("no first output")
    };
    // 60 lines and the prompt on 24 rows: 37 lines are above the screen.
    // Each is drawn once, in order, before the screen's own rows.
    let text = String::from_utf8_lossy(&first).into_owned();
    let drawn = |n: usize| text.match_indices(&format!("answer line {n}\x1b[0m")).map(|(at, _)| at).collect::<Vec<_>>();
    let mut before = 0;
    for n in 1..=60 {
        let at = drawn(n);
        assert_eq!(at.len(), 1, "line {n} is drawn once");
        assert!(at[0] > before, "line {n} follows line {}", n - 1);
        before = at[0];
    }
    assert!(text.find("READY").is_some_and(|ready| ready > before));
    // The terminal ends on the screen the pane shows.
    let mut terminal = crate::screen::ScreenModel::new(24, 80);
    terminal.process(&first);
    let shown = terminal.capture(crate::screen::Capture::Plain);
    assert!(shown.starts_with("answer line 38\n") && shown.contains("answer line 60\nREADY"), "{shown}");
}

#[test]
fn a_client_that_falls_behind_gets_a_repaint_instead() {
    let (host, launcher) = new_host();
    let mut connection = connect(&host);
    connection
        .call::<Created>(create("l", "claude/p/a"))
        .unwrap();
    let session = host.find(&pane("claude/p/a")).unwrap();
    // Attached, but never read: six megabytes of output arrive meanwhile.
    let client = Attachment::new(1, 1, Size { cols: 80, rows: 24 });
    session.attach(client.clone()).unwrap();
    let program = launcher.program(0);
    let chunk = vec![b'x'; 1 << 20];
    for _ in 0..6 {
        program.write(&chunk);
    }
    program.write(b"\r\nEND");
    eventually("the output", || {
        capture(&mut connection, pane("claude/p/a")).contains("END")
    });
    session.detach(client.id);
    let mut total = 0;
    let mut last = Vec::new();
    while let Some(item) = client.next() {
        if let Out::Stream(bytes) = item {
            total += bytes.len();
            last = bytes;
        }
    }
    // What was queued stayed near the limit, and the client still ends up
    // with the whole screen.
    assert!(total < (5 << 20), "{total} bytes queued");
    assert!(String::from_utf8_lossy(&last).contains("END"));
}

#[test]
fn a_session_that_ends_at_once_still_leaves_the_listing() {
    // Its waiter may finish before the create registers it; it must neither
    // stay listed as dead nor block the name or the host's exit.
    let (host, launcher) = new_host();
    launcher.0.exit_at_once.store(true, Ordering::SeqCst);
    let mut client = connect(&host);
    for round in 0..1000 {
        let (launch, name) = (format!("quick-{round}"), format!("claude/p/q{round}"));
        let created: Created = client.call(create(&launch, &name)).unwrap();
        assert!(!created.existing);
        eventually("the end", || {
            client.call::<Listed>(Op::List).unwrap().sessions.is_empty()
        });
        // A retry finds the ended launch and starts nothing.
        let again: Created = client.call(create(&launch, &name)).unwrap();
        assert_eq!((again.pane, again.existing), (created.pane, true));
    }
    assert_eq!(launcher.count(), 1000);
}

fn launch_status(connection: &mut Connection, launch_id: &str) -> Result<LaunchState, ErrorBody> {
    connection.call(Op::LaunchStatus {
        launch_id: launch_id.into(),
    })
}

#[test]
fn a_launch_reports_how_its_session_ended_with_its_last_lines() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    // A launch the host does not know.
    let error = launch_status(&mut client, "launch-x").unwrap_err();
    assert_eq!(
        (error.code, error.message.as_str()),
        (ErrorCode::NotFound, "can't find launch: launch-x")
    );

    // A running session.
    client.call::<Created>(create("l", "claude/p/a")).unwrap();
    let running = LaunchState {
        session: "$0".into(),
        pane: "%0".into(),
        name: "claude/p/a".into(),
        dead: false,
        ended: false,
        code: None,
        output: String::new(),
    };
    assert_eq!(launch_status(&mut client, "l"), Ok(running.clone()));

    // A supervisor that cannot start its agent says why and exits at once;
    // the message, longer than the pane is wide, reads whole.
    let said = "hgs: cannot read the Claude Code version (claude --version: Access is \
                denied. (os error 5)), which decides its tracking hook form";
    *lock(&launcher.0.fail_at_once) = Some((1, format!("\r\n\r\n{said}\r\n").into_bytes()));
    let created: Created = client.call(create("m", "claude/p/b")).unwrap();
    let ended = |client: &mut Connection| launch_status(client, "m").unwrap();
    eventually("the end", || ended(&mut client).ended);
    assert_eq!(
        ended(&mut client),
        LaunchState {
            session: created.session.clone(),
            pane: created.pane.clone(),
            name: "claude/p/b".into(),
            dead: true,
            ended: true,
            code: Some(1),
            output: said.into(),
        }
    );
    // A retried create still finds it, and starts nothing.
    let again: Created = client.call(create("m", "claude/p/b")).unwrap();
    assert_eq!((again.session, again.existing), (created.session, true));
    assert_eq!(launcher.count(), 2);

    // A session that ends later reports its exit too; the other one runs on.
    launcher.program(0).write(b"bye\r\n");
    eventually("the output", || {
        capture(&mut client, pane("claude/p/a")).starts_with("bye")
    });
    launcher.program(0).exit(0);
    eventually("the end", || launch_status(&mut client, "l").unwrap().ended);
    assert_eq!(
        launch_status(&mut client, "l"),
        Ok(LaunchState {
            dead: true,
            ended: true,
            code: Some(0),
            output: "bye".into(),
            ..running
        })
    );
}

#[test]
fn an_ended_session_completes_its_launch_report_before_it_leaves_the_listing() {
    let (host, launcher) = new_host();
    let folder = tempfile::tempdir().unwrap();
    host.complete_launch_reports_in(folder.path().to_owned());
    let mut client = connect(&host);
    let report = |launch: &str| folder.path().join(format!("{launch}.end.json"));
    let read = |launch: &str| std::fs::read_to_string(report(launch)).unwrap();
    let draining = |launch: &str, name: &str| {
        let text = serde_json::json!({"launch_id": launch, "name": name, "code": 1,
                                      "output": "half", "ended_at": 1.5, "draining": true});
        std::fs::write(report(launch), format!("{text}\n")).unwrap();
    };
    let launches = [
        "0b6f4c9e-3c1d-4b8f-9a35-6f2e2d6f6a10",
        "4400baad-7d90-480a-8451-5f3e456e2ae9",
        "92812fe9-eed8-477c-921a-ef3949434722",
    ];
    for (index, launch) in launches.iter().enumerate() {
        let name = format!("claude/p/{index}");
        client.call::<Created>(create(launch, &name)).unwrap();
        launcher.program(index).write(b"half\r\n");
        eventually("the output", || {
            capture(&mut client, pane(&name)).starts_with("half")
        });
    }
    // The supervisor of the first read its screen before the last lines it and
    // its agent wrote had reached the host, and left its report draining.
    draining(launches[0], "claude/p/0");
    *lock(&launcher.program(0).closing) = b"whole\r\nhgs: cannot start\r\n".to_vec();
    launcher.program(0).exit(1);
    eventually("the end", || {
        client.call::<Listed>(Op::List).unwrap().sessions.len() == 2
    });
    // Once a listing misses the session, its report holds the drained lines,
    // as `launch_status` does.
    assert_eq!(
        read(launches[0]),
        format!(
            "{{\"code\":1,\"ended_at\":1.5,\"launch_id\":\"{}\",\"name\":\"claude/p/0\",\
             \"output\":\"half\\nwhole\\nhgs: cannot start\"}}\n",
            launches[0]
        )
    );
    assert_eq!(
        launch_status(&mut client, launches[0]).unwrap().output,
        "half\nwhole\nhgs: cannot start"
    );

    // A complete report (a run that ended after its startup time, a
    // supervisor of an earlier version) stays as it was; a session whose
    // supervisor left none gets none.
    let complete = format!(
        "{{\"code\":1,\"ended_at\":1.5,\"launch_id\":\"{}\",\"name\":\"claude/p/1\",\
         \"output\":\"half\"}}\n",
        launches[1]
    );
    std::fs::write(report(launches[1]), &complete).unwrap();
    for index in [1, 2] {
        *lock(&launcher.program(index).closing) = b"whole\r\n".to_vec();
        launcher.program(index).exit(1);
    }
    eventually("the ends", || {
        client.call::<Listed>(Op::List).unwrap().sessions.is_empty()
    });
    assert_eq!(read(launches[1]), complete);
    assert!(!report(launches[2]).exists());
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 2);
}

#[test]
fn an_ended_screen_keeps_its_last_lines() {
    let screen = "one\n\n  two  \nthree\n\n\n";
    assert_eq!(last_lines(screen, 8, 2048), "one\n  two\nthree");
    assert_eq!(last_lines(screen, 2, 2048), "  two\nthree");
    assert_eq!(last_lines(screen, 8, 7), "o\nthree");
    assert_eq!(last_lines("\n\n", 8, 2048), "");
    // A cut never splits a character.
    assert_eq!(last_lines("aé", 8, 1), "");
    assert_eq!(last_lines("aé", 8, 2), "é");
}

#[test]
fn a_rename_cannot_take_the_name_of_a_session_being_created() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    client.call::<Created>(create("l", "claude/p/a")).unwrap();
    launcher.hold(true);
    let creating = {
        let mut other = connect(&host);
        thread::spawn(move || other.call::<Created>(create("m", "claude/p/b")))
    };
    eventually("the launch", || {
        launcher.0.waiting.load(Ordering::SeqCst) == 1
    });
    let refused = client
        .call::<Done>(Op::Rename {
            target: pane("claude/p/a"),
            name: "claude/p/b".into(),
        })
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Exists);
    launcher.hold(false);
    assert_eq!(creating.join().unwrap().unwrap().name, "claude/p/b");
    let mut names: Vec<String> = client
        .call::<Listed>(Op::List)
        .unwrap()
        .sessions
        .into_iter()
        .map(|session| session.name)
        .collect();
    names.sort();
    assert_eq!(names, ["claude/p/a", "claude/p/b"]);
}

#[test]
fn an_exiting_host_admits_no_hello_and_answers_the_connections_it_had() {
    let (host, _launcher) = new_host();
    let mut admitted = connect(&host);
    host.wait_for_exit(Duration::from_millis(20));
    assert!(host.exiting());
    // A hello after the decision gets no answer: its client looks for the
    // next host, as if this one were gone.
    assert!(matches!(
        Connection::hello(raw_client(&host, CLIENT_PID), Role::Cli),
        Err(ClientError::Closed)
    ));
    // The host waits for the connection it had accepted, which is answered.
    let draining = {
        let host = host.clone();
        thread::spawn(move || host.drain(LIMIT))
    };
    thread::sleep(Duration::from_millis(100));
    assert!(!draining.is_finished(), "an admitted connection is open");
    assert!(admitted
        .call::<Listed>(Op::List)
        .unwrap()
        .sessions
        .is_empty());
    assert_eq!(
        admitted
            .call::<Created>(create("late", "claude/p/late"))
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    drop(admitted);
    let started = Instant::now();
    draining.join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_supervisor_registers_while_its_create_is_still_registering_the_session() {
    // The host resumes the root process before it registers the session, so a
    // quick supervisor asks for a session that is not listed yet.
    let (host, launcher) = new_host();
    launcher.hold(true);
    let creating = {
        let mut client = connect(&host);
        thread::spawn(move || client.call::<Created>(create("l", "claude/p/a")))
    };
    eventually("the launch", || {
        launcher.0.waiting.load(Ordering::SeqCst) == 1
    });
    let registering = {
        let host = host.clone();
        thread::spawn(move || {
            connect_as(&host, 1000, Role::Supervisor)
                .register_supervisor(SupervisorRegistration {
                    session: "$0".into(),
                    pane: "%0".into(),
                    pid: 1000,
                })
                .map(drop)
        })
    };
    thread::sleep(Duration::from_millis(200));
    assert!(!registering.is_finished(), "it waits for the create");
    launcher.hold(false);
    creating.join().unwrap().unwrap();
    registering.join().unwrap().unwrap();

    // Without a create under way an unknown session is refused at once.
    let started = Instant::now();
    let unknown = connect_as(&host, 1000, Role::Supervisor)
        .register_supervisor(SupervisorRegistration {
            session: "$5".into(),
            pane: "%5".into(),
            pid: 1000,
        })
        .err()
        .unwrap();
    assert_eq!(unknown.code, ErrorCode::NotFound);
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// The supervisor's real end of the channel (`host::supervisor`) over a fake
/// console: what its evidence reports, and what a pause does.
struct SupervisedConsole {
    agent_alive: AtomicBool,
    /// Line input, as a shell or an agent that crashed in cooked mode leaves it.
    cooked: AtomicBool,
    program: Arc<Program>,
    pauses: AtomicU32,
}

impl supervisor::Console for SupervisedConsole {
    fn evidence(&self) -> io::Result<ConsoleEvidence> {
        let alive = self.agent_alive.load(Ordering::SeqCst);
        let mut processes = vec![ConsoleProcess {
            pid: 1000,
            name: "hgs.exe".into(),
        }];
        if alive {
            processes.push(ConsoleProcess {
                pid: 2000,
                name: "fake_agent.exe".into(),
            });
        }
        let mode = if self.cooked.load(Ordering::SeqCst) {
            supervisor::ENABLE_LINE_INPUT | supervisor::ENABLE_ECHO_INPUT
        } else {
            0
        } | supervisor::ENABLE_PROCESSED_INPUT;
        Ok(supervisor::evidence(
            mode,
            processes,
            Some(supervisor::Agent {
                pid: 2000,
                alive,
                in_job: alive,
            }),
        ))
    }

    fn pause(&self) -> io::Result<()> {
        // Ctrl+Break ends the agent; the supervisor records the exit and ends.
        self.pauses.fetch_add(1, Ordering::SeqCst);
        self.agent_alive.store(false, Ordering::SeqCst);
        self.program.exit(0xC000_013A);
        Ok(())
    }
}

/// `program`'s supervisor (pid 1000) registered and answering with `console`.
fn real_supervisor(
    host: &Arc<Host<FakeLauncher>>,
    console: Arc<SupervisedConsole>,
) -> thread::JoinHandle<()> {
    let link = connect_as(host, 1000, Role::Supervisor)
        .register_supervisor(SupervisorRegistration {
            session: "$0".into(),
            pane: "%0".into(),
            pid: 1000,
        })
        .unwrap();
    thread::spawn(move || {
        let _ = supervisor::serve(&*link, &*console);
    })
}

#[test]
fn the_supervisor_end_reports_its_console_and_pauses_its_agent() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    client.call::<Created>(create("l", "claude/p/a")).unwrap();
    let program = launcher.program(0);
    client
        .call::<Done>(Op::SetEnvironment {
            target: pane("claude/p/a"),
            name: RUN_MARKER.into(),
            value: Some("run-1".into()),
        })
        .unwrap();
    program.write(b"\x1b[?2004h> ");
    eventually("bracketed paste", || {
        info(&mut client, pane("claude/p/a")).bracketed_paste
    });
    let console = Arc::new(SupervisedConsole {
        agent_alive: AtomicBool::new(true),
        cooked: AtomicBool::new(false),
        program: program.clone(),
        pauses: AtomicU32::new(0),
    });
    let supervisor = real_supervisor(&host, console.clone());
    let claim = || {
        Op::InputState(InputClaim {
            target: Target::Pane("%0".into()),
            name: "claude/p/a".into(),
            run_id: "run-1".into(),
            agent: "claude".into(),
            pid: Some(2000),
        })
    };
    // Raw input with only the agent attached; processed input does not count.
    let ready: InputReady = client.call(claim()).unwrap();
    assert_eq!((ready.cursor_x, ready.cursor_y), (2, 0));
    console.cooked.store(true, Ordering::SeqCst);
    assert_eq!(
        client.call::<InputReady>(claim()).unwrap_err().message,
        NOT_RAW
    );
    console.cooked.store(false, Ordering::SeqCst);
    // The agent crashed and its raw mode lingers while the supervisor runs on.
    console.agent_alive.store(false, Ordering::SeqCst);
    assert_eq!(
        client.call::<InputReady>(claim()).unwrap_err().message,
        NOT_ATTACHED
    );
    console.agent_alive.store(true, Ordering::SeqCst);
    client.call::<InputReady>(claim()).unwrap();

    // A pause is Ctrl+Break for the supervisor's group; the session then ends.
    client
        .call::<Done>(Op::Signal {
            target: pane("claude/p/a"),
            signal: Signal::Pause,
        })
        .unwrap();
    assert_eq!(console.pauses.load(Ordering::SeqCst), 1);
    eventually("the end", || {
        client.call::<Listed>(Op::List).unwrap().sessions.is_empty()
    });
    // The host closed the channel with the session.
    supervisor.join().unwrap();
}

/// Session `index`'s supervisor (pid `1000 + index`), registered: on the
/// first stop request it ends `program` when `ends_agent`, then leaves
/// without an answer, as a supervisor that exits with its agent does.
fn leaving_supervisor(
    host: &Arc<Host<FakeLauncher>>,
    index: u32,
    program: Arc<Program>,
    ends_agent: bool,
) -> thread::JoinHandle<()> {
    let link = connect_as(host, 1000 + index, Role::Supervisor)
        .register_supervisor(SupervisorRegistration {
            session: format!("${index}"),
            pane: format!("%{index}"),
            pid: 1000 + index,
        })
        .unwrap();
    thread::spawn(move || {
        while let Ok(Some(frame)) = link.read_frame() {
            let request = super::protocol::Request::<SupervisorOp>::decode(&frame.payload);
            if request.is_ok_and(|request| request.op == SupervisorOp::Stop) {
                if ends_agent {
                    program.exit(0xC000_013A);
                }
                break;
            }
        }
        // Dropping the only handle ends the connection, as an exit does.
        drop(link);
    })
}

#[test]
fn a_pause_whose_supervisor_leaves_unanswered_counts_only_when_the_session_ends() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    client.call::<Created>(create("l1", "claude/p/a")).unwrap();
    client.call::<Created>(create("l2", "claude/p/b")).unwrap();
    let pause = |client: &mut Connection, name: &str| {
        client.call::<Done>(Op::Signal {
            target: pane(name),
            signal: Signal::Pause,
        })
    };

    // Ctrl+Break ended the agent and the supervisor with it before its answer
    // went out: the pause did what it asks.
    let ended = leaving_supervisor(&host, 0, launcher.program(0), true);
    pause(&mut client, "claude/p/a").unwrap();
    ended.join().unwrap();
    eventually("the end", || {
        client
            .call::<Listed>(Op::List)
            .unwrap()
            .sessions
            .iter()
            .all(|session| session.name != "claude/p/a")
    });

    // A supervisor that leaves while its session runs on has paused nothing.
    let left = leaving_supervisor(&host, 1, launcher.program(1), false);
    let started = Instant::now();
    let error = pause(&mut client, "claude/p/b").unwrap_err();
    left.join().unwrap();
    assert_eq!(error.code, ErrorCode::Refused, "{error:?}");
    assert!(error.message.contains("no longer connected"), "{error:?}");
    assert!(started.elapsed() < LIMIT);
    assert!(lock(&launcher.program(1).exit).is_none(), "still running");
    client
        .call::<Done>(Op::Kill {
            target: pane("claude/p/b"),
        })
        .unwrap();
    eventually("the end", || {
        client.call::<Listed>(Op::List).unwrap().sessions.is_empty()
    });
}

#[test]
fn the_recovery_worker_is_supervised_once_asked_and_stops_with_the_host() {
    // A host without a worker to supervise says so.
    let (host, _launcher) = new_host();
    let mut client = connect(&host);
    assert_eq!(
        client.call::<Done>(Op::RecoveryWorker).unwrap_err().code,
        ErrorCode::Unsupported
    );

    let starts = Arc::new(AtomicU32::new(0));
    let spawned = Arc::new(AtomicU32::new(0));
    let killed = Arc::new(AtomicBool::new(false));
    struct Running(Arc<AtomicBool>);
    impl worker::Process for Running {
        fn wait(&mut self, timeout: Duration) -> io::Result<Option<u32>> {
            thread::sleep(timeout.min(Duration::from_millis(5)));
            Ok(None)
        }
        fn kill(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    {
        let (starts, spawned, killed) = (starts.clone(), spawned.clone(), killed.clone());
        host.supervise_recovery_with(move || {
            starts.fetch_add(1, Ordering::SeqCst);
            let (spawned, killed) = (spawned.clone(), killed.clone());
            worker::Worker::start("test-recovery", move || {
                spawned.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Running(killed.clone())) as Box<dyn worker::Process>)
            })
            .map_err(|error| error.to_string())
        });
    }
    client.call::<Done>(Op::RecoveryWorker).unwrap();
    client.call::<Done>(Op::RecoveryWorker).unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 1, "started once");
    eventually("the worker", || spawned.load(Ordering::SeqCst) == 1);
    assert!(!killed.load(Ordering::SeqCst));
    host.stop_workers();
    assert!(
        killed.load(Ordering::SeqCst),
        "the worker ends with the host"
    );
    assert_eq!(
        client.call::<Done>(Op::RecoveryWorker).unwrap_err().code,
        ErrorCode::Busy
    );
    assert_eq!(starts.load(Ordering::SeqCst), 1);

    // A start that fails is reported.
    let (host, _launcher) = new_host();
    host.supervise_recovery_with(|| Err("the stable hgs CLI is not installed".into()));
    let error = connect(&host).call::<Done>(Op::RecoveryWorker).unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal);
    assert!(error.message.contains("not installed"), "{error:?}");
}

/// `state::backend::host`, the terminal backend hgs uses on Windows, over a
/// real host: every backend operation is answered by the registry.
mod terminal_backend {
    use super::*;
    use crate::{
        host::{
            attach::Terminal,
            client::AttachStream,
            protocol::{Frame, FrameError, HostStatus, Response},
            spawn::Environment,
        },
        state::backend::{
            self as terminal,
            host::{Connector, Failure, HostBackend, HOST_DIED, STARTUP},
            LaunchSpec, PaneField, StageErrors, TerminalBackend, RUN_CHANGED,
        },
    };
    use serde_json::Value;

    struct Direct {
        host: Arc<Host<FakeLauncher>>,
        creates: Arc<AtomicU32>,
        /// The console of this process, when it has one.
        screen: Option<Arc<Screen>>,
    }

    impl Connector for Direct {
        fn call(&self, op: Op) -> Result<Value, Failure> {
            connect(&self.host).request(op).map_err(Failure::Host)
        }
        fn create(&self, op: Op) -> Result<Value, Failure> {
            self.creates.fetch_add(1, Ordering::SeqCst);
            self.call(op)
        }
        fn attach(&self, request: Attach) -> Result<(Attached, AttachStream), Failure> {
            connect_as(&self.host, CLIENT_PID, Role::Attach)
                .attach(request)
                .map_err(Failure::Host)
        }
        fn terminal(&self) -> Option<Box<dyn Terminal>> {
            self.screen
                .clone()
                .map(|screen| Box::new(FakeTerminal(screen)) as Box<dyn Terminal>)
        }
        fn environment(&self) -> Result<Environment, String> {
            // A launch from inside another session.
            Environment::parse([
                "Path=/bin",
                "HGS_SESSION=claude/outer/main",
                "HGS_HOST_PANE=%9",
            ])
            .map_err(|error| error.to_string())
        }
        fn stable_cli(&self) -> Result<String, String> {
            Ok(PROGRAM.into())
        }
        fn parent(&self, pid: u32) -> Option<u32> {
            // The fake supervisor of the first session started agent 2000.
            (pid == 2000).then_some(1000)
        }
        fn var(&self, _: &str) -> Option<String> {
            None
        }
    }

    const LAUNCH_ID: &str = "0b6f4c9e-3c1d-4b8f-9a35-6f2e2d6f6a10";

    #[test]
    fn the_terminal_backend_runs_sessions_through_the_host() {
        let (host, launcher) = new_host();
        let creates = Arc::new(AtomicU32::new(0));
        let backend = HostBackend::new(Direct {
            host: host.clone(),
            creates: creates.clone(),
            screen: None,
        });
        assert!(backend.listing().unwrap().sessions.is_empty());
        assert!(!backend.session_exists("fake_agent/p/a"));
        let spec = LaunchSpec {
            name: "fake_agent/p/a".into(),
            cwd: WORK.into(),
            env: strings(&["HGS_CLIENT=laptop", &format!("HGS_LAUNCH_ID={LAUNCH_ID}")]),
            command: strings(&[PROGRAM, "--run", "0", "fake_agent"]),
            detached: true,
        };
        backend.start(&spec).unwrap();
        assert_eq!(creates.load(Ordering::SeqCst), 1);
        let program = launcher.program(0);
        assert_eq!(program.argv, spec.command);
        assert_eq!(program.cwd, WORK);
        let variable = |name: &str| {
            program
                .environment
                .iter()
                .find(|(key, _)| key == name)
                .and_then(|(_, value)| value.clone())
        };
        assert_eq!(variable("HGS_EXECUTABLE").as_deref(), Some(PROGRAM));
        assert_eq!(variable("HGS_CLIENT").as_deref(), Some("laptop"));
        assert_eq!(variable("HGS_HOST_PANE").as_deref(), Some("%0"));
        assert_eq!(variable("HGS_SESSION"), None);
        assert_eq!(variable("Path").as_deref(), Some("/bin"));
        // A retried start of the same launch starts nothing.
        backend.start(&spec).unwrap();
        assert_eq!(launcher.count(), 1);

        let listing = backend.listing().unwrap();
        assert_eq!(listing.sessions.len(), 1);
        assert_eq!(
            (
                listing.sessions[0].name.as_str(),
                listing.sessions[0].launch_id.as_str()
            ),
            ("fake_agent/p/a", LAUNCH_ID)
        );
        backend.set_run_marker("fake_agent/p/a", "run-1").unwrap();
        let panes = backend.panes().unwrap();
        assert_eq!(
            (
                panes[0].pane.as_str(),
                panes[0].dead.as_str(),
                panes[0].run_marker.as_str()
            ),
            ("%0", "0", "run-1")
        );
        let info = backend
            .pane_info(
                terminal::Target::Name("fake_agent/p/a"),
                &[
                    PaneField::SessionId,
                    PaneField::PanePid,
                    PaneField::Tty,
                    PaneField::RunMarker,
                ],
            )
            .unwrap();
        assert_eq!(info.fields(), ["$0", "1000", "conpty:%0", "run-1"]);
        // The viewing peer is the session variable that attaching sets; the
        // launch's HGS_CLIENT is in the program's environment only.
        assert_eq!(backend.viewer("fake_agent/p/a"), "");
        backend.set_client(terminal::Target::Session("$0"), "laptop");
        assert_eq!(backend.viewer("fake_agent/p/a"), "laptop");
        assert!(backend.on_terminal("2000", "conpty:%0").unwrap());
        assert!(!backend.on_terminal("2001", "conpty:%0").unwrap());

        program.write(b"fake> ");
        eventually("the prompt", || {
            backend
                .capture("%0", terminal::Capture::Plain)
                .unwrap()
                .starts_with(b"fake>\n")
        });
        backend
            .send(
                "%0",
                &[terminal::Key::Literal("ls"), terminal::Key::Named("Enter")],
            )
            .unwrap();
        assert_eq!(program.input_until(b"\r"), b"ls\r");
        program.write(b"\x1b[?2004h");
        eventually("bracketed paste", || {
            backend
                .pane_info(terminal::Target::Pane("%0"), &[PaneField::BracketedPaste])
                .unwrap()
                .value()
                == "1"
        });
        assert!(backend
            .stage("hgs-input-1", b"two\nlines", StageErrors::Read)
            .unwrap());
        backend
            .paste("hgs-input-1", "%0", terminal::Paste::Composer)
            .unwrap();
        assert_eq!(
            program.input_until(b"\x1b[201~"),
            b"\x1b[200~two\nlines\x1b[201~"
        );

        // Input checks need the session's supervisor, which this one never registered.
        let error = backend
            .input_state(&terminal::InputClaim {
                pane: "%0",
                name: "fake_agent/p/a",
                run_id: "run-1",
                agent: "claude",
                pid: Some(2000),
            })
            .err()
            .unwrap();
        assert!(error.contains("no supervisor"), "{error}");

        assert_eq!(
            backend.session_name("$0").unwrap().as_deref(),
            Some("fake_agent/p/a")
        );
        backend.rename("$0", "fake_agent/p/b").unwrap();
        assert!(
            backend.session_exists("fake_agent/p/b") && !backend.session_exists("fake_agent/p/a")
        );
        assert_eq!(backend.session_name("$7").unwrap(), None);

        // Without a supervisor, terminate ends the session as kill-session does.
        backend
            .kill_session(terminal::Target::Session("$0"))
            .unwrap();
        eventually("the end", || backend.listing().unwrap().sessions.is_empty());
        assert!(!backend.session_exists("fake_agent/p/b"));
        assert_eq!(backend.attached_counts().unwrap(), None);
    }

    /// The attach client's console as a test drives it: what is typed, what
    /// the client shows, the window's size and the attach modes.
    #[derive(Default)]
    struct Screen {
        typed: Mutex<VecDeque<io::Result<Vec<u8>>>>,
        typing: Condvar,
        shown: Mutex<String>,
        size: Mutex<Option<Size>>,
        entered: AtomicBool,
        leaves: AtomicU32,
        /// The console cannot take the attach modes (a legacy console).
        refuses: AtomicBool,
    }

    impl Screen {
        fn new(size: Size) -> Arc<Self> {
            let screen = Self::default();
            *lock(&screen.size) = Some(size);
            Arc::new(screen)
        }

        fn keys(&self, bytes: &[u8]) {
            lock(&self.typed).push_back(Ok(bytes.to_vec()));
            self.typing.notify_all();
        }

        /// The console goes away.
        fn fail(&self) {
            lock(&self.typed).push_back(Err(io::Error::other("the console closed")));
            self.typing.notify_all();
        }

        fn shown(&self) -> String {
            lock(&self.shown).clone()
        }

        fn resize(&self, size: Size) {
            *lock(&self.size) = Some(size);
        }
    }

    struct FakeTerminal(Arc<Screen>);

    impl Terminal for FakeTerminal {
        fn size(&self) -> Option<Size> {
            *lock(&self.0.size)
        }
        fn enter(&self) -> io::Result<()> {
            if self.0.refuses.load(Ordering::SeqCst) {
                return Err(io::Error::other("no terminal input sequences"));
            }
            self.0.entered.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn leave(&self) {
            if self.0.entered.swap(false, Ordering::SeqCst) {
                self.0.leaves.fetch_add(1, Ordering::SeqCst);
            }
        }
        fn read(&self, timeout: Duration) -> io::Result<Vec<u8>> {
            let deadline = Instant::now() + timeout;
            let mut typed = lock(&self.0.typed);
            loop {
                if let Some(keys) = typed.pop_front() {
                    return keys;
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Ok(Vec::new());
                }
                typed = self
                    .0
                    .typing
                    .wait_timeout(typed, left)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }
        fn write(&self, text: &str) -> io::Result<()> {
            lock(&self.0.shown).push_str(text);
            Ok(())
        }
    }

    type Exit = Result<(i32, bool), (i32, String)>;

    fn ended(result: crate::cli::Result<terminal::ClientExit>) -> Exit {
        result
            .map(|exit| (exit.code, exit.server_died))
            .map_err(|error| (error.code, error.message))
    }

    fn attached_clients(host: &Arc<Host<FakeLauncher>>) -> u64 {
        connect(host)
            .call::<Listed>(Op::List)
            .unwrap()
            .sessions
            .first()
            .map_or(0, |session| session.attached)
    }

    fn console_backend(
        host: &Arc<Host<FakeLauncher>>,
        size: Size,
    ) -> (Arc<HostBackend<Direct>>, Arc<Screen>, Arc<AtomicU32>) {
        let screen = Screen::new(size);
        let creates = Arc::new(AtomicU32::new(0));
        let backend = Arc::new(HostBackend::new(Direct {
            host: host.clone(),
            creates: creates.clone(),
            screen: Some(screen.clone()),
        }));
        (backend, screen, creates)
    }

    fn attach_in_thread(
        backend: &Arc<HostBackend<Direct>>,
        target: &'static str,
        run_id: Option<&'static str>,
    ) -> thread::JoinHandle<Exit> {
        let backend = backend.clone();
        thread::spawn(move || ended(backend.attach(terminal::Target::Name(target), run_id)))
    }

    #[test]
    fn the_attach_client_relays_keys_and_sizes_until_a_detach() {
        let (host, launcher) = new_host();
        let mut client = connect(&host);
        client.call::<Created>(create("l", "claude/p/a")).unwrap();
        let program = launcher.program(0);
        program.write(b"screen text\r\n\x1b[?2004h");
        eventually("the screen", || {
            capture(&mut client, pane("claude/p/a")).starts_with("screen text")
        });
        let (backend, screen, _) = console_backend(&host, Size { cols: 90, rows: 25 });
        let attached = attach_in_thread(&backend, "claude/p/a", None);

        // A repaint at the client's size, in the attach modes; the listing
        // names the client by its opaque ID (D13).
        eventually("the repaint", || screen.shown().contains("screen text"));
        assert!(screen.entered.load(Ordering::SeqCst));
        assert_eq!(program.sizes().last(), Some(&Size { cols: 90, rows: 25 }));
        eventually("the client", || attached_clients(&host) == 1);
        let listing = backend.listing().unwrap();
        assert_eq!(
            listing
                .clients
                .iter()
                .map(|client| (client.session.as_str(), client.terminal.as_str()))
                .collect::<Vec<_>>(),
            [("claude/p/a", "c1")]
        );

        // Typing, the prefix sent as a key, and another key after the prefix.
        screen.keys(b"hi");
        assert_eq!(program.input_until(b"hi"), b"hi");
        screen.keys(b"\x02\x02");
        screen.keys(b"\x02x");
        assert_eq!(program.input_until(b"x"), b"\x02\x02x");
        // The pane follows the window's size.
        screen.resize(Size {
            cols: 100,
            rows: 30,
        });
        eventually("the resize", || {
            program.sizes().last()
                == Some(&Size {
                    cols: 100,
                    rows: 30,
                })
        });
        // A character the console writes in two pieces shows whole.
        let wide = "\u{4e2d}".as_bytes();
        program.write(&wide[..2]);
        thread::sleep(Duration::from_millis(30));
        program.write(&wide[2..]);
        eventually("the character", || screen.shown().contains('\u{4e2d}'));
        assert!(!screen.shown().contains('\u{fffd}'));

        // Ctrl+B d detaches: the terminal is reset, tmux's line shows, the
        // modes come back and the session runs on without the d.
        screen.keys(b"\x02d");
        assert_eq!(attached.join().unwrap(), Ok((0, false)));
        let shown = screen.shown();
        assert!(
            shown.ends_with("\x1b[30;1H\r\n[detached (from session claude/p/a)]\r\n"),
            "{shown:?}"
        );
        assert!(shown.contains("\x1b[?2004l") && !shown.contains("\x1b[?1049l"));
        assert!(!screen.entered.load(Ordering::SeqCst));
        assert_eq!(screen.leaves.load(Ordering::SeqCst), 1);
        eventually("the detach", || attached_clients(&host) == 0);
        assert_eq!(program.read_input(Duration::from_millis(100)), b"");
        assert!(lock(&program.exit).is_none());
    }

    #[test]
    fn runs_detach_keys_and_the_session_end_decide_how_a_client_ends() {
        let (host, launcher) = new_host();
        let mut client = connect(&host);
        client.call::<Created>(create("l", "claude/p/a")).unwrap();
        client
            .call::<Done>(Op::SetEnvironment {
                target: pane("claude/p/a"),
                name: RUN_MARKER.into(),
                value: Some("run-1".into()),
            })
            .unwrap();
        let program = launcher.program(0);
        let (backend, screen, _) = console_backend(&host, Size { cols: 80, rows: 24 });

        // `--existing --run-id` of another run: refused as the CLI refuses it.
        assert_eq!(
            ended(backend.attach(terminal::Target::Session("$0"), Some("run-2"))),
            Err((1, RUN_CHANGED.into()))
        );
        assert!(!screen.entered.load(Ordering::SeqCst));
        assert_eq!(attached_clients(&host), 0);
        // A session that does not exist: the host's message and 1, as a
        // failed tmux attach.
        assert_eq!(
            ended(backend.attach(terminal::Target::Name("claude/p/none"), None)),
            Ok((1, false))
        );

        // The run that runs: Ctrl+B Ctrl+D detaches.
        let attached = attach_in_thread(&backend, "claude/p/a", Some("run-1"));
        eventually("the client", || attached_clients(&host) == 1);
        screen.keys(b"\x02\x04");
        assert_eq!(attached.join().unwrap(), Ok((0, false)));
        assert!(screen
            .shown()
            .ends_with("[detached (from session claude/p/a)]\r\n"));

        // A console that goes away detaches the client and fails.
        let attached = attach_in_thread(&backend, "claude/p/a", None);
        eventually("the client", || attached_clients(&host) == 1);
        screen.fail();
        let (code, message) = attached.join().unwrap().unwrap_err();
        assert_eq!(code, 1);
        assert!(message.contains("the console closed"), "{message}");
        eventually("the detach", || attached_clients(&host) == 0);
        assert!(!screen.entered.load(Ordering::SeqCst));

        // The session's end ends the client as tmux's [exited].
        let attached = attach_in_thread(&backend, "claude/p/a", None);
        eventually("the client", || attached_clients(&host) == 1);
        program.write(b"\x1b[?1049hfull screen");
        eventually("the output", || screen.shown().contains("full screen"));
        program.exit(3);
        assert_eq!(attached.join().unwrap(), Ok((0, false)));
        let shown = screen.shown();
        assert!(shown.ends_with("[exited]\r\n"), "{shown:?}");
        assert!(shown.contains("\x1b[?1049l\x1b[r"), "{shown:?}");
        // Every attach, the refused ones included, gave its modes back.
        assert_eq!(screen.leaves.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn an_attached_launch_starts_at_the_console_size() {
        let (host, launcher) = new_host();
        let (backend, screen, creates) = console_backend(
            &host,
            Size {
                cols: 120,
                rows: 40,
            },
        );
        let launch = |backend: &Arc<HostBackend<Direct>>, tag: &str| {
            let backend = backend.clone();
            let spec = LaunchSpec {
                name: format!("fake_agent/p/{tag}"),
                cwd: WORK.into(),
                env: Vec::new(),
                command: strings(&[PROGRAM, "--run", "0", "fake_agent"]),
                detached: false,
            };
            thread::spawn(move || ended(backend.launch(&spec)))
        };
        let launched = launch(&backend, "a");
        eventually("the program", || launcher.count() == 1);
        let program = launcher.program(0);
        assert_eq!(
            program.sizes()[0],
            Size {
                cols: 120,
                rows: 40
            }
        );
        eventually("the client", || attached_clients(&host) == 1);
        program.write(b"fake> ");
        eventually("the prompt", || screen.shown().contains("fake> "));
        program.exit(0);
        assert_eq!(launched.join().unwrap(), Ok((0, false)));
        assert!(screen.shown().ends_with("[exited]\r\n"));
        assert_eq!(creates.load(Ordering::SeqCst), 1);
        eventually("the end", || {
            connect(&host)
                .call::<Listed>(Op::List)
                .unwrap()
                .sessions
                .is_empty()
        });

        // A program that ends at once has exited when its client comes.
        launcher.0.exit_at_once.store(true, Ordering::SeqCst);
        assert_eq!(launch(&backend, "b").join().unwrap(), Ok((0, false)));
        assert_eq!(launcher.count(), 2);

        // A console without the attach modes starts nothing and attaches to
        // nothing, as one that is not a console.
        launcher.0.exit_at_once.store(false, Ordering::SeqCst);
        backend.attachable().unwrap();
        assert!(!screen.entered.load(Ordering::SeqCst));
        screen.refuses.store(true, Ordering::SeqCst);
        let refused = launch(&backend, "c").join().unwrap();
        assert_eq!(refused, Err((1, "no terminal input sequences".into())));
        let refused = backend.attachable().unwrap_err();
        assert_eq!(
            (refused.code, refused.message.as_str()),
            (1, "no terminal input sequences")
        );
        assert_eq!((creates.load(Ordering::SeqCst), launcher.count()), (2, 2));
        connect(&host)
            .call::<Created>(create("l", "fake_agent/p/d"))
            .unwrap();
        let refused = ended(backend.attach(terminal::Target::Name("fake_agent/p/d"), None));
        assert_eq!(refused, Err((1, "no terminal input sequences".into())));
        assert_eq!(attached_clients(&host), 0);
        assert!(!screen.entered.load(Ordering::SeqCst));
    }

    #[test]
    fn a_launch_whose_program_cannot_start_fails_with_what_it_said() {
        let (host, launcher) = new_host();
        let startup = Duration::from_millis(300);
        let backend = HostBackend::new(Direct {
            host: host.clone(),
            creates: Arc::default(),
            screen: None,
        })
        .with_startup(startup);
        let spec = |tag: &str, detached| LaunchSpec {
            name: format!("fake_agent/p/{tag}"),
            cwd: WORK.into(),
            env: Vec::new(),
            command: strings(&[PROGRAM, "--run", "0", "fake_agent"]),
            detached,
        };
        let cannot_start = || {
            *lock(&launcher.0.fail_at_once) = Some((
                126,
                b"hgs: fake_agent: Access is denied. (os error 5)\r\n".to_vec(),
            ));
        };
        let denied = |tag: &str| {
            Err((
                1,
                format!(
                    "session fake_agent/p/{tag} ended immediately (exit 126):\n\
                     hgs: fake_agent: Access is denied. (os error 5)"
                ),
            ))
        };

        // The supervisor cannot start its program: a detached launch fails.
        cannot_start();
        assert_eq!(ended(backend.launch(&spec("a", true))), denied("a"));
        // A program that runs for the startup time has started.
        let since = Instant::now();
        assert_eq!(ended(backend.launch(&spec("b", true))), Ok((0, false)));
        assert!(since.elapsed() >= startup);
        // So has one that ends at once with exit 0, as under tmux.
        launcher.0.exit_at_once.store(true, Ordering::SeqCst);
        assert_eq!(ended(backend.launch(&spec("c", true))), Ok((0, false)));
        launcher.0.exit_at_once.store(false, Ordering::SeqCst);
        // A start (resume, restore, fork) does not wait.
        cannot_start();
        backend.start(&spec("d", true)).unwrap();
        assert_eq!(launcher.count(), 4);

        // An attached launch, whose client finds the session ended, fails the
        // same way.
        let (backend, screen, _) = console_backend(&host, Size { cols: 80, rows: 24 });
        cannot_start();
        assert_eq!(ended(backend.launch(&spec("e", false))), denied("e"));
        assert_eq!(launcher.count(), 5);
        // So does one whose client sees the program fail at once.
        let attached = {
            let (backend, spec) = (backend.clone(), spec("f", false));
            thread::spawn(move || ended(backend.launch(&spec)))
        };
        eventually("the client", || {
            let listed: Listed = connect(&host).call(Op::List).unwrap();
            listed
                .sessions
                .iter()
                .any(|session| session.name == "fake_agent/p/f" && session.attached == 1)
        });
        let program = launcher.program(5);
        program.write(b"fake_agent: unknown option --x\r\n");
        eventually("the output", || screen.shown().contains("unknown option"));
        program.exit(2);
        assert_eq!(
            attached.join().unwrap(),
            Err((
                1,
                "session fake_agent/p/f ended immediately (exit 2):\n\
                 fake_agent: unknown option --x"
                    .into()
            ))
        );
        assert!(screen.shown().ends_with("[exited]\r\n"));

        // A client left at once from a program that runs has started; the
        // session runs on.
        let attached_to = |name: &str| {
            let listed: Listed = connect(&host).call(Op::List).unwrap();
            listed
                .sessions
                .iter()
                .any(|session| session.name == name && session.attached == 1)
        };
        let left = {
            let (backend, spec) = (backend.clone(), spec("g", false));
            thread::spawn(move || ended(backend.launch(&spec)))
        };
        eventually("the client", || attached_to("fake_agent/p/g"));
        let since = Instant::now();
        screen.keys(b"\x02\x04");
        assert_eq!(left.join().unwrap(), Ok((0, false)));
        assert!(since.elapsed() < STARTUP, "the launch waited");
        let listed: Listed = connect(&host).call(Op::List).unwrap();
        assert!(listed
            .sessions
            .iter()
            .any(|session| session.name == "fake_agent/p/g"));
        // So has a program that fails only after the startup time.
        let backend = Arc::new(
            HostBackend::new(Direct {
                host: host.clone(),
                creates: Arc::default(),
                screen: Some(screen.clone()),
            })
            .with_startup(startup),
        );
        let late = {
            let (backend, spec) = (backend.clone(), spec("h", false));
            thread::spawn(move || ended(backend.launch(&spec)))
        };
        eventually("the client", || attached_to("fake_agent/p/h"));
        // The launch counts its startup time from the moment its session was
        // created, before its client attached; a clock started before the
        // launch ends early when creating the session is slow under load.
        thread::sleep(startup);
        launcher.program(7).exit(2);
        assert_eq!(late.join().unwrap(), Ok((0, false)));
        assert_eq!(launcher.count(), 8);
    }

    /// A host that answers one attach, shows a screen, reads one key and then
    /// dies. `running` says whether a host answers afterwards.
    struct Dying {
        screen: Arc<Screen>,
        running: bool,
    }

    impl Connector for Dying {
        fn call(&self, op: Op) -> Result<Value, Failure> {
            assert_eq!(op, Op::HostStatus);
            if !self.running {
                return Err(Failure::NoHost);
            }
            Ok(serde_json::to_value(HostStatus {
                sessions: 1,
                clients: 0,
                conpty: "inbox".into(),
            })
            .unwrap())
        }
        fn create(&self, _: Op) -> Result<Value, Failure> {
            unreachable!("nothing is created")
        }
        fn attach(&self, request: Attach) -> Result<(Attached, AttachStream), Failure> {
            let (client, host) = memory_pair();
            thread::spawn(move || {
                host.read_frame().unwrap().unwrap();
                let hello = HelloReply {
                    protocol: 1,
                    ..identity()
                };
                write_json(&host, Kind::Json, &HelloAnswer::Accepted(hello)).unwrap();
                host.read_frame().unwrap().unwrap();
                let attached = Attached {
                    client: "c1".into(),
                    session: "$0".into(),
                    name: "claude/p/a".into(),
                    pane: "%0".into(),
                };
                write_json(&host, Kind::Json, &Response::ok(1, &attached)).unwrap();
                host.write_frame(Kind::Stream, b"\x1b[?1049hagent screen")
                    .unwrap();
                let typed = host.read_frame().unwrap().unwrap();
                assert_eq!(
                    (typed.kind, typed.payload.as_slice()),
                    (Kind::Stream, &b"x"[..])
                );
            });
            Connection::hello(Arc::new(client), Role::Attach)
                .map_err(|error| Failure::Unreachable(error.to_string()))?
                .attach(request)
                .map_err(Failure::Host)
        }
        fn terminal(&self) -> Option<Box<dyn Terminal>> {
            Some(Box::new(FakeTerminal(self.screen.clone())))
        }
        fn environment(&self) -> Result<Environment, String> {
            unreachable!("nothing is created")
        }
        fn stable_cli(&self) -> Result<String, String> {
            unreachable!("nothing is created")
        }
        fn parent(&self, _: u32) -> Option<u32> {
            None
        }
        fn var(&self, _: &str) -> Option<String> {
            None
        }
    }

    #[test]
    fn a_lost_host_restores_the_terminal_and_a_dead_one_is_reported() {
        assert!(HOST_DIED.ends_with("terminal restored, sessions on this box are gone"));
        for running in [false, true] {
            let screen = Screen::new(Size { cols: 80, rows: 24 });
            let backend = HostBackend::new(Dying {
                screen: screen.clone(),
                running,
            });
            let attached = {
                thread::spawn(move || {
                    ended(backend.attach(terminal::Target::Name("claude/p/a"), None))
                })
            };
            eventually("the screen", || screen.shown().contains("agent screen"));
            screen.keys(b"x");
            // Only a host that no longer runs died, as tmux's server.
            assert_eq!(attached.join().unwrap(), Ok((1, !running)));
            let shown = screen.shown();
            assert!(shown.contains("agent screen\x1b[?1049l\x1b[r"), "{shown:?}");
            assert!(!shown.contains("[detached") && !shown.contains("[exited]"));
            assert!(!screen.entered.load(Ordering::SeqCst));
            assert_eq!(screen.leaves.load(Ordering::SeqCst), 1);
        }
    }

    /// A client end whose control frame (the detach) is answered at once by
    /// the end of the stream, before the write returns: the host closes an
    /// attached connection as soon as it reads a detach.
    struct ClosedOnDetach<L: Link>(L);

    impl<L: Link> Link for ClosedOnDetach<L> {
        fn read_frame(&self) -> Result<Option<Frame>, FrameError> {
            self.0.read_frame()
        }
        fn write_frame(&self, kind: Kind, payload: &[u8]) -> Result<(), FrameError> {
            self.0.write_frame(kind, payload)?;
            if kind == Kind::Control {
                self.0.shutdown();
                thread::sleep(Duration::from_millis(300));
            }
            Ok(())
        }
        fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            self.0.set_read_timeout(timeout)
        }
        fn shutdown(&self) {
            self.0.shutdown();
        }
    }

    #[test]
    fn a_detach_the_host_closes_at_once_is_still_a_detach() {
        let (client, host) = memory_pair();
        let host = thread::spawn(move || {
            host.read_frame().unwrap().unwrap();
            let hello = HelloReply {
                protocol: 1,
                ..identity()
            };
            write_json(&host, Kind::Json, &HelloAnswer::Accepted(hello)).unwrap();
            host.read_frame().unwrap().unwrap();
            let attached = Attached {
                client: "c1".into(),
                session: "$0".into(),
                name: "claude/p/a".into(),
                pane: "%0".into(),
            };
            write_json(&host, Kind::Json, &Response::ok(1, &attached)).unwrap();
            while let Ok(Some(_)) = host.read_frame() {}
        });
        let size = Size { cols: 80, rows: 24 };
        let (attached, stream) = Connection::hello(Arc::new(ClosedOnDetach(client)), Role::Attach)
            .unwrap()
            .attach(Attach {
                target: Target::Name("claude/p/a".into()),
                size,
                run_id: None,
            })
            .unwrap();
        let screen = Screen::new(size);
        let terminal = FakeTerminal(screen.clone());
        terminal.enter().unwrap();
        screen.keys(b"\x02d");
        let outcome = crate::host::attach::relay(&stream, &terminal, &attached.name, size);
        assert_eq!(outcome, crate::host::attach::Outcome::Detached);
        assert!(screen
            .shown()
            .ends_with("[detached (from session claude/p/a)]\r\n"));
        drop(stream);
        host.join().unwrap();
    }
}

/// Task #33: a process a Git Bash wrapper started in a session is not the
/// session's descendant any more (the wrapper's process ended), but it is
/// still in the session's job. The host names that session for the
/// connection's own client, with its run marker; a process outside every
/// session hears not_found.
#[test]
fn the_host_names_the_session_whose_job_holds_the_asking_process() {
    let (host, launcher) = new_host();
    let mut client = connect(&host);
    let _: Created = client.call(create("launch-a", "claude/p/a")).unwrap();
    let _: Created = client.call(create("launch-b", "claude/p/b")).unwrap();
    client
        .call::<Done>(Op::SetEnvironment {
            target: pane("claude/p/a"),
            name: RUN_MARKER.into(),
            value: Some("run-a".into()),
        })
        .unwrap();
    lock(&launcher.program(0).members).push(4242);
    let found: CallerSession = connect_as(&host, 4242, Role::Cli).call(Op::CallerSession).unwrap();
    assert_eq!(
        found,
        CallerSession {
            session: "$0".into(),
            name: "claude/p/a".into(),
            pane: "%0".into(),
            run_marker: "run-a".into(),
        }
    );
    // A session's root process is its own.
    let root: CallerSession = connect_as(&host, 1001, Role::Cli).call(Op::CallerSession).unwrap();
    assert_eq!((root.name.as_str(), root.run_marker.as_str()), ("claude/p/b", ""));
    // Outside every session.
    let outside = connect_as(&host, 5555, Role::Cli).call::<CallerSession>(Op::CallerSession).unwrap_err();
    assert_eq!(outside.code, ErrorCode::NotFound);
}
