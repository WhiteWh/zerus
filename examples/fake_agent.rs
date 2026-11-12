//! A native stand-in for an interactive agent in hgs terminal tests. It never
//! calls a model and is never installed or packaged. Tests run it under tmux on
//! Linux and macOS and under the session host's pseudoconsole on Windows, where
//! a script cannot be started as an agent (Windows Phase 2, milestone M7). It
//! records what reaches it, draws what a test gives it and reacts to terminal,
//! hook and signal events in a configured way; `tests/test_fake_agent.py` covers
//! it under a private tmux socket.
//!
//! Started as `fake_agent` (any name that begins with `fake_agent` or
//! `fake-agent`), it reads `--option VALUE` (or `--option=VALUE`) arguments and
//! keeps everything after `--` as agent arguments. A copy or link with another
//! name, such as `claude`, `codex` or `kimi` on a test PATH, treats every
//! argument as the agent's own and is configured only by the environment. Each
//! option is also the variable `FAKE_AGENT_<NAME>` (upper case, `-` as `_`);
//! arguments override the environment, and an empty variable counts as unset.
//! TEXT values understand `\e`, `\r`, `\n`, `\t`, `\a`, `\0`, `\\` and `\xHH`.
//!
//! - `--record PATH`: append one JSON object per line (below).
//! - `--ready PATH`: once started, write `{"pid", "process_start", "cwd",
//!   "argv", "session_id", ...}` there (replaced atomically). `process_start` is
//!   the form hgs records: `ps -o lstart=` with `LC_ALL=C TZ=UTC` on Unix, the
//!   creation time in decimal 100 ns ticks on Windows. The `start` record carries
//!   the same process identity and folder.
//! - `--raw 1|0` (default 1): switch the terminal to raw input. On Unix this is
//!   `cfmakeraw` (no `ICANON`, `ECHO`, `ISIG`, `IXON` or `OPOST`); on Windows it
//!   clears `ENABLE_LINE_INPUT`, `ENABLE_ECHO_INPUT` and `ENABLE_PROCESSED_INPUT`
//!   and sets `ENABLE_VIRTUAL_TERMINAL_INPUT`. Output on Windows always gets
//!   `ENABLE_VIRTUAL_TERMINAL_PROCESSING` and `DISABLE_NEWLINE_AUTO_RETURN`.
//! - `--modes LIST` (default `paste`): modes to set at start, in order. Names:
//!   `paste` (?2004), `alt` (?1049), `cursor` (?1, application cursor keys),
//!   `keypad` (DECKPAM), `mouse` (?1000), `mouse-button` (?1002), `mouse-any`
//!   (?1003), `mouse-sgr` (?1006), `focus` (?1004); `no-NAME` resets one and
//!   `none` is nothing. Modes still set are reset on exit unless `--reset-modes 0`.
//! - `--render PATH`: write these bytes to the terminal after the modes, in steps
//!   of `--render-chunk N` bytes (0, the default, is one write) with
//!   `--render-delay MS` between them; then `--prompt TEXT`.
//! - `--echo 1`: echo typed text and pastes; Enter writes CR LF and the prompt.
//! - `--query LIST`: terminal queries to send at start: `dsr` (CSI 6n), `status`
//!   (CSI 5n), `da1` (CSI c), `da2` (CSI > c), `xtversion` (CSI > 0 q), `osc10`
//!   and `osc11` (colour queries). Their answers are recorded as `reply`. With
//!   `--query-wait MS` it waits up to MS for every answer before it is ready,
//!   records `query-timeout` for missing ones, and with `--query-fail-code N`
//!   then exits with N, like an agent that cannot start without an answer.
//! - `--esc-ms MS` (default 30): how long an incomplete escape sequence waits for
//!   the rest; a lone ESC is recorded when nothing follows.
//! - `--exit-keys TEXT` and `--exit-code N` (default 0): exit when the input
//!   outside pastes ends with these bytes.
//! - `--on-eof POLICY` (default `exit:0`): what the end of input does, as for a
//!   signal below except `default`. An exit a signal already scheduled is kept.
//!   A closing pane or pseudoconsole ends the input as it sends SIGHUP or
//!   `CTRL_CLOSE`, in either order; `--on-eof ignore` leaves the exit to that
//!   signal's policy.
//! - `--on-signal NAME=POLICY[,...]` (repeatable): `INT`, `TERM`, `HUP` (Unix) and
//!   `CTRL_C`, `CTRL_BREAK`, `CTRL_CLOSE` (Windows) are recorded as `signal`, then
//!   handled by POLICY: `exit[:CODE]`, `delay:MS[:CODE]` (keep running, exit
//!   after MS), `ignore`, or `default` (the system default; on Unix it is not
//!   recorded). By default each exits with 128 + its number on Unix and with
//!   `STATUS_CONTROL_C_EXIT` on Windows. Windows ends a process at most about
//!   5 s after `CTRL_CLOSE`, whatever its policy. Ctrl+C reaches a raw terminal
//!   as input, not as a signal; the agent leaves the inherited Windows Ctrl+C
//!   setting as it is, so a test sees what its parent arranged.
//! - Hooks run with the event JSON on stdin, as agents run them: SessionStart
//!   when ready, UserPromptSubmit when a later Enter ends a non-empty prompt,
//!   Stop `--turn-ms MS` later (default 0; `never` waits for a control command) and
//!   SessionEnd on exit. `--hooks claude|codex|kimi|none` reads the commands that
//!   agent's configuration holds (`$CLAUDE_CONFIG_DIR/settings.json`,
//!   `$CODEX_HOME/hooks.json`, `$KIMI_CODE_HOME/config.toml`, each defaulting to
//!   its folder in the home directory) once at start; the default is the
//!   program name when it is one of these. `--hook-command CMD` adds a command
//!   for every event. `--hook-events LIST` (default `start,submit,stop,end`)
//!   picks the events, `--hook-timeout MS` (default 10000) bounds a hook without
//!   its own `timeout`, and `--hook-extra JSON` adds fields to every payload. On
//!   Unix a command runs with `/bin/sh -c`. On Windows a configured command runs
//!   as its agent runs it (spec D12): Codex's (its `commandWindows` when there is
//!   one) with PowerShell, `pwsh` or else Windows PowerShell, as `-NoProfile
//!   -Command COMMAND` in a console of its own, Kimi's with `%COMSPEC% /d /s /c
//!   "COMMAND"` (Node.js's `shell: true`); Claude Code's and `--hook-command`
//!   directly, the first word (or double-quoted path) as the program and the
//!   rest as its command line. An entry with `args` (Claude Code's exec form)
//!   runs its `command` with those arguments and no shell everywhere. A hook
//!   that exits with 2 skips the rest of that event's hooks.
//! - Payloads carry `session_id`, `hook_event_name`, `cwd`, `source` (`startup`,
//!   or `resume` when the agent arguments resume a conversation), `prompt`,
//!   `reason` (`--end-reason TEXT`, default `other`) and, with
//!   `--transcript-dir DIR`, a `transcript_path` that holds the session ID line.
//!   With Claude's hooks, each submitted prompt is then appended to it as a
//!   `{"type":"user"}` row after UserPromptSubmit, as Claude Code records it.
//! - Ctrl+U deletes the composer's current line, echoed as a cleared row; on an
//!   empty line it does nothing, as Claude Code, Codex and Kimi do.
//! - `--swallow-enter 1` ignores Enter: the composer keeps its text and no
//!   prompt is submitted, as a native agent that swallowed the key leaves it.
//! - `--claude-frame 1` (with `--echo 1`) draws Claude Code's composer frame from
//!   the top of the screen on every change: a full-width rule, `❯ ` and the
//!   composer's lines (continuations indented two spaces), and a rule below,
//!   with the cursor after the text.
//!   `--session-id ID` fixes the ID; otherwise agent arguments `--resume ID`,
//!   `-r ID`, `--session ID`, `resume ID` (resumed) or `--session-id ID` choose
//!   it, else a random UUID.
//! - `--probe-text TEXT`: when the agent arguments are just `--version` or
//!   `--help`, print TEXT and exit 0 at once, recording nothing: the answer to
//!   the capability probes hgs runs before a tracked agent starts.
//! - `--control PATH`: while running, a file appearing there (write it elsewhere
//!   and rename it in) is read, removed and run line by line: `write TEXT`,
//!   `render PATH`, `steps CHUNK DELAY_MS`, `modes LIST`, `query LIST`,
//!   `hook start|submit|stop|end [PROMPT]`, `exit [CODE]`. A `control` record
//!   follows each line. This replaces the SIGUSR1 redraws of the Python fakes.
//!
//! Records have `kind`, `t` (seconds since start) and `pid`, so agents started
//! one after another or side by side can share a record. Input becomes one record
//! per token, carrying the exact bytes as `hex` (all `hex` values together are
//! every byte received, in order), `read` (the number of the read that
//! completed it) and, for printable data, `text`:
//! `text` (a run of printable bytes), `paste` (one bracketed paste: `hex` with
//! its markers, `text` without them, `t0` and `read0` where it began), `enter`
//! (CR), `esc` (a lone ESC, or one before a byte that starts no sequence: only
//! CSI, SS3 and the OSC 10/11 and DCS `>|` shapes of query replies do, since
//! agents read ESC before other text as Alt), `ctrl-c`, `control` (another C0
//! byte or DEL, with `name` such as `ctrl-s`, `tab` or `backspace`), `key`
//! (another escape sequence, such as a cursor key, a mouse or focus report) and
//! `reply` (an answer to a query, with `query`, or `"unsolicited": true` when
//! none was waiting; a cursor position report, shaped like a modified F3 key,
//! counts only once `dsr` was sent). A sequence cut short by the escape
//! timeout or the exit is marked `"incomplete": true`. Other kinds:
//! `start`, `hooks`, `modes`, `query`, `query-timeout`, `render`, `ready`,
//! `hook` (event, command, `args` for an exec form, status, ms, timeout,
//! blocked, stdout, stderr),
//! `signal`, `resize`, `control`, `eof`, `error` and `exit` (code and cause).
use serde_json::{json, Map, Value};
use std::{
    collections::hash_map::RandomState,
    env,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    hash::BuildHasher,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
    sync::{mpsc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";
/// How often control files and (on Windows) the console size are checked.
const TICK: Duration = Duration::from_millis(20);
/// Hook output kept in a record.
const OUTPUT_LIMIT: usize = 4096;

const MODES: &[(&str, &[u8], &[u8])] = &[
    ("paste", b"\x1b[?2004h", b"\x1b[?2004l"),
    ("alt", b"\x1b[?1049h", b"\x1b[?1049l"),
    ("cursor", b"\x1b[?1h", b"\x1b[?1l"),
    ("keypad", b"\x1b=", b"\x1b>"),
    ("mouse", b"\x1b[?1000h", b"\x1b[?1000l"),
    ("mouse-button", b"\x1b[?1002h", b"\x1b[?1002l"),
    ("mouse-any", b"\x1b[?1003h", b"\x1b[?1003l"),
    ("mouse-sgr", b"\x1b[?1006h", b"\x1b[?1006l"),
    ("focus", b"\x1b[?1004h", b"\x1b[?1004l"),
];

const QUERIES: &[(&str, &[u8])] = &[
    ("dsr", b"\x1b[6n"),
    ("status", b"\x1b[5n"),
    ("da1", b"\x1b[c"),
    ("da2", b"\x1b[>c"),
    ("xtversion", b"\x1b[>0q"),
    ("osc10", b"\x1b]10;?\x1b\\"),
    ("osc11", b"\x1b]11;?\x1b\\"),
];

/// Short event names and the hook events agents report for them.
const EVENTS: &[(&str, &str)] = &[
    ("start", "SessionStart"),
    ("submit", "UserPromptSubmit"),
    ("stop", "Stop"),
    ("end", "SessionEnd"),
];

const SIGNALS: &[&str] = &["INT", "TERM", "HUP", "CTRL_C", "CTRL_BREAK", "CTRL_CLOSE"];

const OPTIONS: &[&str] = &[
    "record",
    "ready",
    "raw",
    "modes",
    "render",
    "render-chunk",
    "render-delay",
    "prompt",
    "echo",
    "query",
    "query-wait",
    "query-fail-code",
    "esc-ms",
    "exit-keys",
    "exit-code",
    "on-eof",
    "on-signal",
    "hooks",
    "hook-command",
    "hook-events",
    "hook-timeout",
    "hook-extra",
    "session-id",
    "transcript-dir",
    "turn-ms",
    "end-reason",
    "control",
    "reset-modes",
    "swallow-enter",
    "claude-frame",
    "probe-text",
];

static START: OnceLock<Instant> = OnceLock::new();
static RECORD: Mutex<Option<File>> = Mutex::new(None);
static EVENTS_TX: OnceLock<mpsc::Sender<Event>> = OnceLock::new();
static POLICIES: OnceLock<Vec<(&'static str, Policy)>> = OnceLock::new();

enum Event {
    Input(Vec<u8>, Instant),
    Eof,
    Signal(&'static str),
    /// SIGWINCH; Windows polls the console size instead.
    #[cfg_attr(windows, allow(dead_code))]
    Resize,
}

#[derive(Clone, Copy)]
enum Policy {
    Default,
    Ignore,
    Exit(i32),
    Delay(Duration, i32),
}

impl Policy {
    fn describe(self) -> String {
        match self {
            Policy::Default => "default".into(),
            Policy::Ignore => "ignore".into(),
            Policy::Exit(code) => format!("exit:{code}"),
            Policy::Delay(delay, code) => format!("delay:{}:{code}", delay.as_millis()),
        }
    }
}

struct Config {
    name: String,
    argv: Vec<String>,
    record: Option<PathBuf>,
    ready: Option<PathBuf>,
    raw: bool,
    modes: Vec<(&'static str, bool)>,
    render: Option<PathBuf>,
    render_chunk: usize,
    render_delay: Duration,
    prompt: Vec<u8>,
    echo: bool,
    queries: Vec<&'static str>,
    query_wait: Duration,
    query_fail_code: Option<i32>,
    esc: Duration,
    exit_keys: Vec<u8>,
    exit_code: i32,
    on_eof: Policy,
    policies: Vec<(&'static str, Policy)>,
    hooks: Option<Option<String>>,
    hook_command: Option<String>,
    hook_events: Vec<&'static str>,
    hook_timeout: Duration,
    hook_extra: Map<String, Value>,
    session_id: Option<String>,
    transcript_dir: Option<PathBuf>,
    turn: Option<Duration>,
    end_reason: String,
    control: Option<PathBuf>,
    reset_modes: bool,
    swallow_enter: bool,
    claude_frame: bool,
    probe_text: Option<String>,
}

impl Config {
    fn new(name: String) -> Config {
        Config {
            name,
            argv: Vec::new(),
            record: None,
            ready: None,
            raw: true,
            modes: vec![("paste", true)],
            render: None,
            render_chunk: 0,
            render_delay: Duration::ZERO,
            prompt: Vec::new(),
            echo: false,
            queries: Vec::new(),
            query_wait: Duration::ZERO,
            query_fail_code: None,
            esc: Duration::from_millis(30),
            exit_keys: Vec::new(),
            exit_code: 0,
            on_eof: Policy::Exit(0),
            policies: Vec::new(),
            hooks: None,
            hook_command: None,
            hook_events: EVENTS.iter().map(|(short, _)| *short).collect(),
            hook_timeout: Duration::from_secs(10),
            hook_extra: Map::new(),
            session_id: None,
            transcript_dir: None,
            turn: Some(Duration::ZERO),
            end_reason: "other".into(),
            control: None,
            reset_modes: true,
            swallow_enter: false,
            claude_frame: false,
            probe_text: None,
        }
    }

    fn set(&mut self, option: &str, value: &str) -> Result<(), String> {
        match option {
            "record" => self.record = Some(value.into()),
            "ready" => self.ready = Some(value.into()),
            "raw" => self.raw = flag(value)?,
            "modes" => self.modes = parse_modes(value)?,
            "render" => self.render = Some(value.into()),
            "render-chunk" => self.render_chunk = number(value)? as usize,
            "render-delay" => self.render_delay = millis(value)?,
            "prompt" => self.prompt = unescape(value)?,
            "echo" => self.echo = flag(value)?,
            "query" => self.queries = parse_queries(value)?,
            "query-wait" => self.query_wait = millis(value)?,
            "query-fail-code" => self.query_fail_code = Some(parse_code(value)?),
            "esc-ms" => self.esc = millis(value)?,
            "exit-keys" => self.exit_keys = unescape(value)?,
            "exit-code" => self.exit_code = parse_code(value)?,
            "on-eof" => {
                self.on_eof = match parse_policy("EOF", value)? {
                    Policy::Default => return Err("the end of input has no default".into()),
                    policy => policy,
                }
            }
            "on-signal" => {
                for item in value
                    .split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                {
                    let (name, policy) = item
                        .split_once('=')
                        .ok_or_else(|| format!("expected NAME=POLICY, not {item:?}"))?;
                    let name = signal_name(name)?;
                    let policy = parse_policy(name, policy)?;
                    self.policies.retain(|(known, _)| *known != name);
                    self.policies.push((name, policy));
                }
            }
            "hooks" => {
                self.hooks = Some(match value {
                    "none" => None,
                    "claude" | "codex" | "kimi" => Some(value.to_owned()),
                    _ => return Err(format!("unknown hook source {value:?}")),
                })
            }
            "hook-command" => self.hook_command = Some(value.to_owned()),
            "hook-events" => {
                self.hook_events = list(value)
                    .map(|name| {
                        EVENTS
                            .iter()
                            .map(|(short, _)| *short)
                            .find(|short| *short == name)
                            .ok_or_else(|| format!("unknown hook event {name:?}"))
                    })
                    .collect::<Result<_, _>>()?
            }
            "hook-timeout" => self.hook_timeout = millis(value)?,
            "hook-extra" => {
                self.hook_extra = match serde_json::from_str(value) {
                    Ok(Value::Object(fields)) => fields,
                    _ => return Err("expected a JSON object".into()),
                }
            }
            "session-id" => self.session_id = Some(value.to_owned()),
            "transcript-dir" => self.transcript_dir = Some(value.into()),
            "turn-ms" => {
                self.turn = if value == "never" {
                    None
                } else {
                    Some(millis(value)?)
                }
            }
            "end-reason" => self.end_reason = value.to_owned(),
            "control" => self.control = Some(value.into()),
            "reset-modes" => self.reset_modes = flag(value)?,
            "swallow-enter" => self.swallow_enter = flag(value)?,
            "claude-frame" => self.claude_frame = flag(value)?,
            "probe-text" => self.probe_text = Some(value.to_owned()),
            _ => return Err(format!("unknown option --{option}")),
        }
        Ok(())
    }

    fn load() -> Result<Config, String> {
        let mut args = env::args_os().map(|arg| arg.to_string_lossy().into_owned());
        let program = args.next().unwrap_or_else(|| "fake_agent".into());
        let args: Vec<String> = args.collect();
        let name = Path::new(&program)
            .file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let name = name.strip_suffix(".exe").unwrap_or(&name).to_owned();
        let options = name.starts_with("fake_agent") || name.starts_with("fake-agent");
        let mut config = Config::new(name);
        for option in OPTIONS {
            let variable = format!("FAKE_AGENT_{}", option.to_uppercase().replace('-', "_"));
            if let Some(value) = env::var_os(&variable).filter(|value| !value.is_empty()) {
                let value = value
                    .into_string()
                    .map_err(|_| format!("{variable} is not valid Unicode"))?;
                config
                    .set(option, &value)
                    .map_err(|error| format!("{variable}: {error}"))?;
            }
        }
        if !options {
            config.argv = args;
            return Ok(config);
        }
        let mut index = 0;
        while let Some(arg) = args.get(index) {
            if arg == "--" {
                config.argv = args[index + 1..].to_vec();
                break;
            }
            let option = arg
                .strip_prefix("--")
                .ok_or_else(|| format!("unexpected argument {arg:?}; agent arguments follow --"))?;
            let (option, value) = match option.split_once('=') {
                Some((option, value)) => (option, value.to_owned()),
                None => {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or_else(|| format!("--{option} needs a value"))?;
                    (option, value.clone())
                }
            };
            config
                .set(option, &value)
                .map_err(|error| format!("--{option}: {error}"))?;
            index += 1;
        }
        Ok(config)
    }
}

fn list(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
}

fn flag(value: &str) -> Result<bool, String> {
    match value {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!("expected 1 or 0, not {value:?}")),
    }
}

fn number(value: &str) -> Result<u64, String> {
    value
        .trim()
        .parse()
        .map_err(|_| format!("expected a non-negative number, not {value:?}"))
}

fn millis(value: &str) -> Result<Duration, String> {
    number(value).map(Duration::from_millis)
}

/// An exit code in decimal or `0x` hexadecimal (for NTSTATUS values).
fn parse_code(value: &str) -> Result<i32, String> {
    let value = value.trim();
    let parsed = match value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        Some(hex) => u32::from_str_radix(hex, 16).ok().map(|code| code as i32),
        None => value.parse().ok(),
    };
    parsed.ok_or_else(|| format!("expected an exit code, not {value:?}"))
}

fn unescape(value: &str) -> Result<Vec<u8>, String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        index += 1;
        if byte != b'\\' {
            out.push(byte);
            continue;
        }
        let escape = *bytes.get(index).ok_or("a trailing backslash")?;
        index += 1;
        out.push(match escape {
            b'e' => 0x1b,
            b'r' => b'\r',
            b'n' => b'\n',
            b't' => b'\t',
            b'a' => 0x07,
            b'0' => 0,
            b'\\' => b'\\',
            b'x' => {
                let digits = value
                    .get(index..index + 2)
                    .ok_or("\\x needs two hex digits")?;
                index += 2;
                u8::from_str_radix(digits, 16).map_err(|_| "\\x needs two hex digits")?
            }
            other => return Err(format!("unknown escape \\{}", other as char)),
        });
    }
    Ok(out)
}

fn parse_modes(value: &str) -> Result<Vec<(&'static str, bool)>, String> {
    list(value)
        .filter(|item| *item != "none")
        .map(|item| {
            let (name, on) = match item.strip_prefix("no-") {
                Some(name) => (name, false),
                None => (item, true),
            };
            MODES
                .iter()
                .find(|(known, _, _)| *known == name)
                .map(|(known, _, _)| (*known, on))
                .ok_or_else(|| format!("unknown mode {item:?}"))
        })
        .collect()
}

fn parse_queries(value: &str) -> Result<Vec<&'static str>, String> {
    list(value)
        .map(|item| {
            QUERIES
                .iter()
                .find(|(known, _)| *known == item)
                .map(|(known, _)| *known)
                .ok_or_else(|| format!("unknown query {item:?}"))
        })
        .collect()
}

fn signal_name(value: &str) -> Result<&'static str, String> {
    let upper = value.trim().to_uppercase().replace('-', "_");
    let bare = upper.strip_prefix("SIG").unwrap_or(&upper);
    SIGNALS
        .iter()
        .find(|known| **known == bare)
        .copied()
        .ok_or_else(|| format!("unknown signal {value:?}"))
}

fn default_code(signal: &str) -> i32 {
    match signal {
        "INT" => 130,
        "TERM" => 143,
        "HUP" => 129,
        "EOF" => 0,
        // STATUS_CONTROL_C_EXIT, as the default console handler exits.
        _ => 0xC000_013Au32 as i32,
    }
}

fn parse_policy(signal: &str, value: &str) -> Result<Policy, String> {
    let mut parts = value.trim().split(':');
    let policy = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("default"), None, None, None) => Policy::Default,
        (Some("ignore"), None, None, None) => Policy::Ignore,
        (Some("exit"), code, None, None) => Policy::Exit(
            code.map(parse_code)
                .transpose()?
                .unwrap_or(default_code(signal)),
        ),
        (Some("delay"), Some(delay), code, None) => Policy::Delay(
            millis(delay)?,
            code.map(parse_code)
                .transpose()?
                .unwrap_or(default_code(signal)),
        ),
        _ => return Err(format!("unknown policy {value:?}")),
    };
    Ok(policy)
}

fn policy_for(signal: &str) -> Policy {
    POLICIES
        .get()
        .and_then(|policies| policies.iter().find(|(name, _)| *name == signal))
        .map(|(_, policy)| *policy)
        .unwrap_or(Policy::Exit(default_code(signal)))
}

/// The conversation the agent arguments name, and whether they resume it.
fn agent_session(argv: &[String]) -> (Option<String>, bool) {
    let mut chosen = (None, false);
    for (index, arg) in argv.iter().enumerate() {
        let next = argv
            .get(index + 1)
            .filter(|value| !value.starts_with('-'))
            .cloned();
        match arg.as_str() {
            "--resume" | "-r" | "--session" | "resume" if next.is_some() => return (next, true),
            "--session-id" if next.is_some() => chosen = (next, false),
            _ => {
                if let Some(id) = arg.strip_prefix("--resume=") {
                    return (Some(id.to_owned()), true);
                }
                if let Some(id) = arg.strip_prefix("--session-id=") {
                    chosen = (Some(id.to_owned()), false);
                }
            }
        }
    }
    chosen
}

fn random_uuid() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let high = RandomState::new().hash_one((nanos, process::id(), 1u8));
    let low = RandomState::new().hash_one((nanos, process::id(), 2u8));
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&high.to_be_bytes());
    bytes[8..].copy_from_slice(&low.to_be_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex(&bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn seconds(at: Instant) -> f64 {
    let start = *START.get_or_init(Instant::now);
    (at.saturating_duration_since(start).as_secs_f64() * 1e6).round() / 1e6
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Append one record line; `t` defaults to now.
fn record(mut value: Value) {
    if value.get("t").is_none() {
        value["t"] = json!(seconds(Instant::now()));
    }
    if value.get("pid").is_none() {
        value["pid"] = json!(process::id());
    }
    let mut guard = RECORD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(file) = guard.as_mut() {
        let mut line = serde_json::to_vec(&value).unwrap_or_default();
        line.push(b'\n');
        let _ = file.write_all(&line);
    }
}

/// A signal or console event arrived: record it and hand it to the main loop.
fn signal_arrived(name: &'static str) {
    record(json!({"kind": "signal", "signal": name, "policy": policy_for(name).describe()}));
    if let Some(sender) = EVENTS_TX.get() {
        let _ = sender.send(Event::Signal(name));
    }
}

fn send(event: Event) {
    if let Some(sender) = EVENTS_TX.get() {
        let _ = sender.send(event);
    }
}

/// Write to the terminal. A vanished terminal is not an error here.
fn out(bytes: &[u8]) {
    // The Windows console accepts only UTF-8 from std; a sequence split across
    // writes is kept by std, invalid bytes become U+FFFD.
    #[cfg(windows)]
    let replaced;
    #[cfg(windows)]
    let bytes = match std::str::from_utf8(bytes) {
        Err(error) if error.error_len().is_some() => {
            replaced = lossy(bytes);
            replaced.as_bytes()
        }
        _ => bytes,
    };
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(bytes);
    let _ = stdout.flush();
}

fn home() -> PathBuf {
    let variables: &[&str] = if cfg!(windows) {
        &["USERPROFILE", "HOME"]
    } else {
        &["HOME"]
    };
    variables
        .iter()
        .filter_map(env::var_os)
        .find(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_default()
}

#[cfg(unix)]
mod platform {
    use super::{send, signal_arrived, Event, Instant, Policy};
    use std::{
        ffi::c_void,
        io::{self, Read},
        mem,
        process::{Command, Stdio},
        sync::{
            atomic::{AtomicI32, Ordering},
            Mutex,
        },
        thread,
    };

    static ORIGINAL: Mutex<Option<libc::termios>> = Mutex::new(None);
    static SIGNAL_PIPE: AtomicI32 = AtomicI32::new(-1);

    pub fn prepare_output() {}

    pub fn make_raw() -> bool {
        // SAFETY: plain termios calls on standard input with valid pointers.
        unsafe {
            let mut original: libc::termios = mem::zeroed();
            if libc::tcgetattr(0, &mut original) != 0 {
                return false;
            }
            let mut raw = original;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(0, libc::TCSANOW, &raw) != 0 {
                return false;
            }
            *ORIGINAL
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(original);
        }
        true
    }

    pub fn restore() {
        if let Some(original) = ORIGINAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            // SAFETY: restores the attributes read from the same descriptor.
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &original) };
        }
    }

    pub fn size() -> Option<(u16, u16)> {
        for fd in [0, 1] {
            // SAFETY: TIOCGWINSZ writes one winsize.
            unsafe {
                let mut size: libc::winsize = mem::zeroed();
                if libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) == 0 && size.ws_col > 0 {
                    return Some((size.ws_col, size.ws_row));
                }
            }
        }
        None
    }

    /// The start time as hgs records it on Unix.
    pub fn process_start() -> Option<String> {
        let output = Command::new("ps")
            .args(["-p", &std::process::id().to_string(), "-o", "lstart="])
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        (output.status.success() && !text.is_empty()).then_some(text)
    }

    pub fn read_input() {
        let mut stdin = io::stdin().lock();
        // Larger than std's buffer, so each read is one read(2).
        let mut buffer = vec![0; 65536];
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => send(Event::Input(buffer[..count].to_vec(), Instant::now())),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        send(Event::Eof);
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn errno() -> *mut libc::c_int {
        // SAFETY: returns the calling thread's errno location.
        unsafe { libc::__errno_location() }
    }
    #[cfg(any(target_vendor = "apple", target_os = "freebsd"))]
    fn errno() -> *mut libc::c_int {
        // SAFETY: returns the calling thread's errno location.
        unsafe { libc::__error() }
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd"
    )))]
    fn errno() -> *mut libc::c_int {
        std::ptr::null_mut()
    }

    extern "C" fn on_signal(signal: libc::c_int) {
        let fd = SIGNAL_PIPE.load(Ordering::Relaxed);
        if fd < 0 {
            return;
        }
        let byte = signal as u8;
        let location = errno();
        // SAFETY: write(2) is async-signal-safe; errno is restored for the
        // interrupted code.
        unsafe {
            let saved = if location.is_null() { 0 } else { *location };
            libc::write(fd, (&byte as *const u8).cast::<c_void>(), 1);
            if !location.is_null() {
                *location = saved;
            }
        }
    }

    pub fn install_signals(policy: fn(&str) -> Policy) {
        let mut fds = [0; 2];
        // SAFETY: a pipe whose write end the handler uses; neither end is
        // inherited by hooks.
        unsafe {
            if libc::pipe(fds.as_mut_ptr()) != 0 {
                return;
            }
            for fd in fds {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
            let flags = libc::fcntl(fds[1], libc::F_GETFL);
            libc::fcntl(fds[1], libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        SIGNAL_PIPE.store(fds[1], Ordering::Relaxed);
        let reader = fds[0];
        thread::spawn(move || loop {
            let mut byte = 0u8;
            // SAFETY: reads one byte into a local.
            let count = unsafe { libc::read(reader, (&mut byte as *mut u8).cast::<c_void>(), 1) };
            if count == 1 {
                match byte as libc::c_int {
                    libc::SIGWINCH => send(Event::Resize),
                    libc::SIGINT => signal_arrived("INT"),
                    libc::SIGTERM => signal_arrived("TERM"),
                    libc::SIGHUP => signal_arrived("HUP"),
                    _ => {}
                }
            } else if count < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            } else {
                break;
            }
        });
        let handled = [
            (libc::SIGINT, Some("INT")),
            (libc::SIGTERM, Some("TERM")),
            (libc::SIGHUP, Some("HUP")),
            (libc::SIGWINCH, None),
        ];
        for (signal, name) in handled {
            // SAFETY: installs a handler that only writes to the pipe, or the
            // system default.
            unsafe {
                if name.is_some_and(|name| matches!(policy(name), Policy::Default)) {
                    libc::signal(signal, libc::SIG_DFL);
                    continue;
                }
                let mut action: libc::sigaction = mem::zeroed();
                action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
                action.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut action.sa_mask);
                libc::sigaction(signal, &action, std::ptr::null_mut());
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::{policy_for, send, signal_arrived, Event, Instant, Policy};
    use std::{io::Read, mem, sync::Mutex, thread, time::Duration};
    use windows_sys::{
        core::BOOL,
        Win32::{
            Foundation::{FILETIME, HANDLE},
            System::{
                Console::{
                    GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle, ReadConsoleW,
                    SetConsoleCtrlHandler, SetConsoleMode, CONSOLE_SCREEN_BUFFER_INFO,
                    CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, DISABLE_NEWLINE_AUTO_RETURN,
                    ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
                    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_HANDLE,
                    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
                },
                Threading::{GetCurrentProcess, GetProcessTimes},
            },
        },
    };

    /// Console modes to restore: input, output.
    static ORIGINAL: Mutex<(Option<u32>, Option<u32>)> = Mutex::new((None, None));

    fn handle(kind: STD_HANDLE) -> HANDLE {
        // SAFETY: GetStdHandle has no preconditions.
        unsafe { GetStdHandle(kind) }
    }

    fn mode(handle: HANDLE) -> Option<u32> {
        let mut mode = 0;
        // SAFETY: writes one mode for a handle that may not be a console.
        (unsafe { GetConsoleMode(handle, &mut mode) } != 0).then_some(mode)
    }

    pub fn prepare_output() {
        let output = handle(STD_OUTPUT_HANDLE);
        if let Some(original) = mode(output) {
            let wanted =
                original | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN;
            // SAFETY: changes the mode of our own console output.
            if unsafe { SetConsoleMode(output, wanted) } != 0 {
                ORIGINAL
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .1 = Some(original);
            }
        }
    }

    pub fn make_raw() -> bool {
        let input = handle(STD_INPUT_HANDLE);
        let Some(original) = mode(input) else {
            return false;
        };
        let raw = (original & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT))
            | ENABLE_VIRTUAL_TERMINAL_INPUT;
        // SAFETY: changes the mode of our own console input.
        if unsafe { SetConsoleMode(input, raw) } == 0 {
            return false;
        }
        ORIGINAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .0 = Some(original);
        true
    }

    pub fn restore() {
        let (input, output) = mem::take(
            &mut *ORIGINAL
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        // SAFETY: restores modes read from the same handles.
        unsafe {
            if let Some(input) = input {
                SetConsoleMode(handle(STD_INPUT_HANDLE), input);
            }
            if let Some(output) = output {
                SetConsoleMode(handle(STD_OUTPUT_HANDLE), output);
            }
        }
    }

    pub fn size() -> Option<(u16, u16)> {
        // SAFETY: fills one plain structure.
        unsafe {
            let mut info: CONSOLE_SCREEN_BUFFER_INFO = mem::zeroed();
            if GetConsoleScreenBufferInfo(handle(STD_OUTPUT_HANDLE), &mut info) == 0 {
                return None;
            }
            let window = info.srWindow;
            Some((
                (window.Right - window.Left + 1) as u16,
                (window.Bottom - window.Top + 1) as u16,
            ))
        }
    }

    /// The creation time in 100 ns ticks, as hgs records it on Windows.
    pub fn process_start() -> Option<String> {
        let empty = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (empty, empty, empty, empty);
        // SAFETY: the pseudo handle of this process and four output structures.
        let ok = unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        };
        (ok != 0).then(|| {
            ((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
                .to_string()
        })
    }

    pub fn read_input() {
        let input = handle(STD_INPUT_HANDLE);
        if mode(input).is_none() {
            // Not a console: plain bytes, as on Unix.
            let mut stdin = std::io::stdin().lock();
            let mut buffer = vec![0; 65536];
            while let Ok(count) = stdin.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                send(Event::Input(buffer[..count].to_vec(), Instant::now()));
            }
            send(Event::Eof);
            return;
        }
        // ReadConsoleW returns UTF-16 without changing the code page; std's
        // console reader would also end the input at Ctrl+Z.
        let mut units = vec![0u16; 4096];
        let mut carry: Option<u16> = None;
        loop {
            let mut count = 0u32;
            // SAFETY: reads at most units.len() UTF-16 units into the buffer.
            let ok = unsafe {
                ReadConsoleW(
                    input,
                    units.as_mut_ptr().cast(),
                    units.len() as u32,
                    &mut count,
                    std::ptr::null(),
                )
            };
            if ok == 0 {
                break;
            }
            if count == 0 {
                thread::sleep(Duration::from_millis(5));
                continue;
            }
            let at = Instant::now();
            let mut text: Vec<u16> = carry.take().into_iter().collect();
            text.extend_from_slice(&units[..count as usize]);
            if text
                .last()
                .is_some_and(|unit| (0xD800..0xDC00).contains(unit))
            {
                carry = text.pop();
            }
            let decoded: String = char::decode_utf16(text)
                .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect();
            if !decoded.is_empty() {
                send(Event::Input(decoded.into_bytes(), at));
            }
        }
        send(Event::Eof);
    }

    unsafe extern "system" fn on_control(kind: u32) -> BOOL {
        let name = match kind {
            CTRL_C_EVENT => "CTRL_C",
            CTRL_BREAK_EVENT => "CTRL_BREAK",
            CTRL_CLOSE_EVENT => "CTRL_CLOSE",
            _ => return 0,
        };
        signal_arrived(name);
        if matches!(policy_for(name), Policy::Default) {
            return 0;
        }
        if kind == CTRL_CLOSE_EVENT {
            // Returning lets Windows end the process at once; the main loop
            // exits by its policy, or Windows ends it after its timeout.
            thread::sleep(Duration::from_secs(30));
        }
        1
    }

    pub fn install_signals(_policy: fn(&str) -> Policy) {
        // SAFETY: registers a handler that stays valid for the process's life.
        unsafe { SetConsoleCtrlHandler(Some(on_control), 1) };
    }
}

/// One framed piece of input.
struct Token {
    kind: &'static str,
    bytes: Vec<u8>,
    at: Instant,
    read: u64,
    /// For a paste: where it began.
    began: Option<(Instant, u64)>,
    incomplete: bool,
}

enum Scan {
    Token(usize, &'static str),
    Incomplete,
    PasteStart,
}

/// Splits input into tokens. Sequences that may continue in the next read
/// wait in `pending`; a bracketed paste collects until its end marker.
#[derive(Default)]
struct Tokenizer {
    pending: Vec<u8>,
    pending_since: Option<(Instant, u64)>,
    paste: Option<(Vec<u8>, Instant, u64)>,
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn scan(bytes: &[u8]) -> Scan {
    match bytes[0] {
        0x1b => scan_escape(bytes),
        0x03 => Scan::Token(1, "ctrl-c"),
        b'\r' => Scan::Token(1, "enter"),
        0x00..=0x1f | 0x7f => Scan::Token(1, "control"),
        _ => {
            let length = bytes
                .iter()
                .position(|byte| *byte < 0x20 || *byte == 0x7f)
                .unwrap_or(bytes.len());
            match std::str::from_utf8(&bytes[..length]) {
                // A character cut by the end of this read waits for the rest.
                Err(error) if error.error_len().is_none() && length == bytes.len() => {
                    if error.valid_up_to() == 0 {
                        Scan::Incomplete
                    } else {
                        Scan::Token(error.valid_up_to(), "text")
                    }
                }
                _ => Scan::Token(length, "text"),
            }
        }
    }
}

fn scan_escape(bytes: &[u8]) -> Scan {
    let Some(&next) = bytes.get(1) else {
        return Scan::Incomplete;
    };
    match next {
        b'[' => {
            let mut end = 2;
            while bytes
                .get(end)
                .is_some_and(|byte| (0x20..=0x3f).contains(byte))
            {
                end += 1;
            }
            let Some(last) = bytes.get(end) else {
                return Scan::Incomplete;
            };
            if !(0x40..=0x7e).contains(last) {
                return Scan::Token(1, "esc");
            }
            let length = end + 1;
            if &bytes[..length] == PASTE_START {
                return Scan::PasteStart;
            }
            // A legacy mouse report carries three raw bytes after CSI M.
            if &bytes[..length] == b"\x1b[M" {
                return if bytes.len() >= 6 {
                    Scan::Token(6, "csi")
                } else {
                    Scan::Incomplete
                };
            }
            Scan::Token(length, "csi")
        }
        b'O' => {
            if bytes.len() >= 3 {
                Scan::Token(3, "key")
            } else {
                Scan::Incomplete
            }
        }
        // OSC (ended by BEL or ST) and DCS (ended by ST), only in the shapes of
        // the replies to its queries: agents read ESC before other text as Alt,
        // so a typed ESC followed by `Please` stays an `esc` and text.
        b']' | b'P' => {
            const REPLIES: &[&[u8]] = &[b"]10;", b"]11;", b"P>|"];
            let rest = &bytes[1..];
            if !REPLIES.iter().any(|reply| rest.starts_with(reply)) {
                return if REPLIES.iter().any(|reply| reply.starts_with(rest)) {
                    Scan::Incomplete
                } else {
                    Scan::Token(1, "esc")
                };
            }
            let mut index = 2;
            while let Some(&byte) = bytes.get(index) {
                if byte == 0x07 && next == b']' {
                    return Scan::Token(index + 1, "string");
                }
                if byte == 0x1b {
                    return match bytes.get(index + 1) {
                        Some(b'\\') => Scan::Token(index + 2, "string"),
                        Some(_) => Scan::Token(index, "string"),
                        None => Scan::Incomplete,
                    };
                }
                index += 1;
            }
            Scan::Incomplete
        }
        _ => Scan::Token(1, "esc"),
    }
}

impl Tokenizer {
    /// A sequence still waiting after this read waits from this read on.
    fn feed(&mut self, bytes: &[u8], at: Instant, read: u64, tokens: &mut Vec<Token>) {
        self.pending.extend_from_slice(bytes);
        self.drain(at, read, false, tokens);
    }

    /// Emit what waits, as it is: after the escape timeout or at exit.
    fn flush(&mut self, tokens: &mut Vec<Token>) {
        if let Some((at, read)) = self.pending_since {
            self.drain(at, read, true, tokens);
        }
    }

    fn drain(&mut self, at: Instant, read: u64, flush: bool, tokens: &mut Vec<Token>) {
        let bytes = std::mem::take(&mut self.pending);
        self.pending_since = None;
        let mut index = 0;
        let token = |kind, bytes: &[u8], incomplete| Token {
            kind,
            bytes: bytes.to_vec(),
            at,
            read,
            began: None,
            incomplete,
        };
        while index < bytes.len() {
            if let Some((content, _, _)) = &mut self.paste {
                let before = content.len();
                content.extend_from_slice(&bytes[index..]);
                let from = before.saturating_sub(PASTE_END.len() - 1);
                let Some(end) = find(&content[from..], PASTE_END).map(|end| end + from) else {
                    return;
                };
                index += end + PASTE_END.len() - before;
                content.truncate(end);
                let (content, began_at, began_read) = self.paste.take().unwrap();
                let mut framed = PASTE_START.to_vec();
                framed.extend_from_slice(&content);
                framed.extend_from_slice(PASTE_END);
                tokens.push(Token {
                    began: Some((began_at, began_read)),
                    ..token("paste", &framed, false)
                });
                continue;
            }
            match scan(&bytes[index..]) {
                Scan::Incomplete if !flush => {
                    self.pending = bytes[index..].to_vec();
                    self.pending_since = Some((at, read));
                    return;
                }
                Scan::Incomplete => {
                    let rest = &bytes[index..];
                    let kind = match rest[0] {
                        0x1b if rest.len() == 1 => "esc",
                        0x1b => "key",
                        _ => "text",
                    };
                    tokens.push(token(kind, rest, kind != "esc"));
                    index = bytes.len();
                }
                Scan::PasteStart => {
                    self.paste = Some((Vec::new(), at, read));
                    index += PASTE_START.len();
                }
                Scan::Token(length, kind) => {
                    tokens.push(token(kind, &bytes[index..index + length], false));
                    index += length;
                }
            }
        }
    }

    /// The moment a waiting sequence is given up on.
    fn deadline(&self, wait: Duration) -> Option<Instant> {
        if self.pending.is_empty() {
            return None;
        }
        self.pending_since.map(|(at, _)| at + wait)
    }
}

fn control_name(byte: u8) -> String {
    match byte {
        0x09 => "tab".into(),
        0x7f => "backspace".into(),
        _ => format!("ctrl-{}", ((byte + 0x40) as char).to_ascii_lowercase()),
    }
}

#[derive(Clone)]
struct Hook {
    event: String,
    command: String,
    /// Claude Code's exec form: run `command` with these, without a shell.
    args: Option<Vec<String>>,
    /// The agent whose way of running a command this hook follows.
    runner: Runner,
    timeout: Option<Duration>,
}

/// How a hook's command runs (on Windows; Unix uses `/bin/sh -c`).
#[derive(Clone, Copy)]
enum Runner {
    /// Directly: Claude Code's shell form and `--hook-command`.
    Direct,
    /// `-NoProfile -Command COMMAND` with the session's PowerShell, as Codex
    /// runs it.
    Codex,
    /// `%COMSPEC% /d /s /c "COMMAND"`, as Kimi Code (Node.js) runs it.
    Kimi,
}

/// Hook commands from an agent's own configuration, as it would run them.
fn load_hooks(agent: &str) -> (Vec<Hook>, Value) {
    let (variable, folder, file) = match agent {
        "claude" => ("CLAUDE_CONFIG_DIR", ".claude", "settings.json"),
        "codex" => ("CODEX_HOME", ".codex", "hooks.json"),
        _ => ("KIMI_CODE_HOME", ".kimi-code", "config.toml"),
    };
    let directory = env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(folder));
    let path = directory.join(file);
    let mut info = json!({"kind": "hooks", "source": agent, "path": path.to_string_lossy()});
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            info["error"] = json!(error.to_string());
            return (Vec::new(), info);
        }
    };
    let timeout = |seconds: Option<f64>| {
        seconds
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
            .map(Duration::from_secs_f64)
    };
    let mut hooks = Vec::new();
    if text.trim().is_empty() {
        info["count"] = json!(0);
        return (hooks, info);
    }
    if agent == "kimi" {
        match text.parse::<toml_edit::DocumentMut>() {
            Ok(document) => {
                let tables = document
                    .get("hooks")
                    .and_then(toml_edit::Item::as_array_of_tables);
                for table in tables.into_iter().flat_map(|tables| tables.iter()) {
                    let field = |key: &str| table.get(key).and_then(toml_edit::Item::as_str);
                    if let (Some(event), Some(command)) = (field("event"), field("command")) {
                        hooks.push(Hook {
                            event: event.to_owned(),
                            command: command.to_owned(),
                            args: None,
                            runner: Runner::Kimi,
                            timeout: timeout(
                                table
                                    .get("timeout")
                                    .and_then(toml_edit::Item::as_integer)
                                    .map(|value| value as f64),
                            ),
                        });
                    }
                }
            }
            Err(error) => info["error"] = json!(error.to_string()),
        }
    } else {
        match serde_json::from_str::<Value>(&text) {
            Ok(document) => {
                let events = document.get("hooks").and_then(Value::as_object);
                for (event, groups) in events.into_iter().flatten() {
                    let groups = groups.as_array().into_iter().flatten();
                    let entries = groups
                        .filter_map(|group| group.get("hooks").and_then(Value::as_array))
                        .flatten();
                    for entry in entries {
                        let kind = entry
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("command");
                        let text = |key: &str| entry.get(key).and_then(Value::as_str);
                        // Codex prefers its Windows command there.
                        let windows = (cfg!(windows) && agent == "codex")
                            .then(|| text("commandWindows").or_else(|| text("command_windows")))
                            .flatten();
                        let args = entry.get("args").and_then(Value::as_array).map(|args| {
                            args.iter()
                                .map(|arg| arg.as_str().unwrap_or_default().to_owned())
                                .collect()
                        });
                        if let (Some(command), "command") = (windows.or(text("command")), kind) {
                            hooks.push(Hook {
                                event: event.clone(),
                                command: command.to_owned(),
                                args,
                                runner: if agent == "codex" {
                                    Runner::Codex
                                } else {
                                    Runner::Direct
                                },
                                timeout: timeout(entry.get("timeout").and_then(Value::as_f64)),
                            });
                        }
                    }
                }
            }
            Err(error) => info["error"] = json!(error.to_string()),
        }
    }
    info["count"] = json!(hooks.len());
    (hooks, info)
}

#[cfg(unix)]
fn hook_process(command: &str, _runner: Runner) -> Command {
    let mut process = Command::new("/bin/sh");
    process.arg("-c").arg(command);
    process
}

/// The PowerShell Codex runs a hook with on Windows (its `default_user_shell`):
/// `pwsh` on PATH or in PowerShell 7's folder, else Windows PowerShell.
#[cfg(windows)]
fn codex_powershell() -> PathBuf {
    let on_path = |name: &str| {
        env::split_paths(&env::var_os("PATH").unwrap_or_default())
            .map(|folder| folder.join(name))
            .find(|path| path.is_file())
    };
    let system = PathBuf::from(env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()));
    on_path("pwsh.exe")
        .or_else(|| {
            Some(PathBuf::from(r"C:\Program Files\PowerShell\7\pwsh.exe")).filter(|p| p.is_file())
        })
        .or_else(|| on_path("powershell.exe"))
        .unwrap_or_else(|| system.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"))
}

/// `"C:/path/hgs.exe" __state hook` as `runner` starts it: directly (the
/// program, then its command line as is), through PowerShell as Codex does or
/// through `cmd.exe` as Kimi Code does.
#[cfg(windows)]
fn hook_process(command: &str, runner: Runner) -> Command {
    use std::os::windows::process::CommandExt;
    /// CREATE_NO_WINDOW, as Codex starts its hooks.
    const NO_WINDOW: u32 = 0x0800_0000;
    let shell = || Command::new(env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into()));
    match runner {
        Runner::Direct => {}
        Runner::Codex => {
            let mut process = Command::new(codex_powershell());
            process
                .args(["-NoProfile", "-Command", command])
                .creation_flags(NO_WINDOW);
            return process;
        }
        Runner::Kimi => {
            let mut process = shell();
            process
                .args(["/d", "/s", "/c"])
                .raw_arg(format!("\"{command}\""));
            return process;
        }
    }
    let command = command.trim_start();
    let (program, rest) = match command.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"').unwrap_or((quoted, "")),
        None => command
            .split_once(char::is_whitespace)
            .unwrap_or((command, "")),
    };
    let mut process = Command::new(program);
    let rest = rest.trim_start();
    if !rest.is_empty() {
        process.raw_arg(rest);
    }
    process
}

fn clip(bytes: &[u8]) -> String {
    lossy(&bytes[..bytes.len().min(OUTPUT_LIMIT)])
}

/// Run one hook with the payload on stdin; its fields for the record.
fn run_hook(hook: &Hook, payload: &[u8], timeout: Duration) -> Map<String, Value> {
    let started = Instant::now();
    let mut fields = Map::new();
    let mut process = match &hook.args {
        Some(args) => {
            let mut process = Command::new(&hook.command);
            process.args(args);
            process
        }
        None => hook_process(&hook.command, hook.runner),
    };
    let spawned = process
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            fields.insert("error".into(), json!(error.to_string()));
            return fields;
        }
    };
    let stdin = child.stdin.take();
    let payload = payload.to_vec();
    thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(&payload);
        }
    });
    let (sender, receiver) = mpsc::channel();
    let streams: [Option<Box<dyn Read + Send>>; 2] = [
        child
            .stdout
            .take()
            .map(|stream| Box::new(stream) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|stream| Box::new(stream) as Box<dyn Read + Send>),
    ];
    for (index, stream) in streams.into_iter().enumerate() {
        let sender = sender.clone();
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut stream) = stream {
                let _ = stream.read_to_end(&mut bytes);
            }
            let _ = sender.send((index, bytes));
        });
    }
    drop(sender);
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= timeout => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().ok();
            }
            Ok(None) => thread::sleep(Duration::from_millis(2)),
            Err(_) => break None,
        }
    };
    let mut output = [Vec::new(), Vec::new()];
    // A grandchild may keep the pipes open; do not wait for it.
    let grace = Instant::now() + Duration::from_millis(200);
    while let Ok((index, bytes)) =
        receiver.recv_timeout(grace.saturating_duration_since(Instant::now()))
    {
        output[index] = bytes;
    }
    fields.insert(
        "status".into(),
        json!(status.and_then(|status| status.code())),
    );
    fields.insert("ms".into(), json!(started.elapsed().as_millis() as u64));
    fields.insert("timeout".into(), json!(timed_out));
    fields.insert("stdout".into(), json!(clip(&output[0])));
    fields.insert("stderr".into(), json!(clip(&output[1])));
    fields
}

struct Agent {
    config: Config,
    events: mpsc::Receiver<Event>,
    tokenizer: Tokenizer,
    reads: u64,
    /// Queries sent and not answered yet, oldest first.
    queries: Vec<&'static str>,
    /// Every query sent so far.
    asked: Vec<&'static str>,
    composer: String,
    /// Modes set now, in the order they were set.
    modes: Vec<&'static str>,
    hooks: Vec<Hook>,
    session_id: String,
    resumed: bool,
    transcript: Option<PathBuf>,
    /// Record each submitted prompt in the transcript as Claude Code does
    /// (zerus-iitw8): set when the hooks are Claude's and a transcript exists.
    claude_rows: bool,
    stop_at: Option<Instant>,
    exit_at: Option<(Instant, i32, String)>,
    exit_now: Option<(i32, String)>,
    recent: Vec<u8>,
    /// SessionStart has run; Enter submits nothing before.
    started: bool,
    size: Option<(u16, u16)>,
    process_start: Option<String>,
    cwd: Option<String>,
}

impl Agent {
    fn run_until(&mut self, deadline: Option<Instant>, done: fn(&Agent) -> bool) {
        loop {
            if let Some((code, cause)) = self.exit_now.take() {
                self.finish(code, &cause);
            }
            let now = Instant::now();
            if let Some((at, code, cause)) = &self.exit_at {
                if now >= *at {
                    let (code, cause) = (*code, cause.clone());
                    self.finish(code, &cause);
                }
            }
            if self.stop_at.is_some_and(|at| now >= at) {
                self.stop_at = None;
                self.hook("stop", None);
            }
            if self
                .tokenizer
                .deadline(self.config.esc)
                .is_some_and(|at| now >= at)
            {
                let mut tokens = Vec::new();
                self.tokenizer.flush(&mut tokens);
                self.tokens(tokens);
            }
            self.poll_control();
            if cfg!(windows) {
                self.check_size();
            }
            if self.exit_now.is_some() {
                continue;
            }
            if done(self) || deadline.is_some_and(|at| now >= at) {
                return;
            }
            let mut wake = now + TICK;
            let deadlines = [
                deadline,
                self.exit_at.as_ref().map(|(at, _, _)| *at),
                self.stop_at,
                self.tokenizer.deadline(self.config.esc),
            ];
            for at in deadlines.into_iter().flatten() {
                wake = wake.min(at);
            }
            match self
                .events
                .recv_timeout(wake.saturating_duration_since(now))
            {
                Ok(Event::Input(bytes, at)) => {
                    self.reads += 1;
                    let mut tokens = Vec::new();
                    self.tokenizer.feed(&bytes, at, self.reads, &mut tokens);
                    self.tokens(tokens);
                }
                Ok(Event::Eof) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    record(json!({"kind": "eof", "policy": self.config.on_eof.describe()}));
                    self.schedule(self.config.on_eof, "eof".into());
                }
                Ok(Event::Signal(name)) => self.signal(name),
                Ok(Event::Resize) => self.check_size(),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn signal(&mut self, name: &'static str) {
        self.schedule(policy_for(name), format!("signal:{name}"));
    }

    /// Exit by a policy. A signal's exit wins over one already scheduled; the
    /// end of input never shortens or replaces a scheduled exit.
    fn schedule(&mut self, policy: Policy, cause: String) {
        let eof = cause == "eof";
        match policy {
            Policy::Exit(code) if !(eof && self.exit_at.is_some()) => {
                self.exit_now = Some((code, cause))
            }
            Policy::Delay(delay, code) if self.exit_at.is_none() => {
                self.exit_at = Some((Instant::now() + delay, code, cause));
            }
            _ => {}
        }
    }

    fn check_size(&mut self) {
        let size = platform::size();
        if size != self.size {
            self.size = size;
            if let Some((cols, rows)) = size {
                record(json!({"kind": "resize", "cols": cols, "rows": rows}));
            }
        }
    }

    /// What an escape sequence or string is, given the queries that wait.
    fn classify(
        &mut self,
        kind: &'static str,
        bytes: &[u8],
    ) -> (&'static str, Option<&'static str>) {
        let query = if kind == "csi" {
            let params = &bytes[2..bytes.len() - 1];
            let last = bytes[bytes.len() - 1];
            let digits = |text: &[u8]| {
                text.iter()
                    .all(|byte| byte.is_ascii_digit() || *byte == b';')
            };
            match last {
                // A modified F3 key has this shape too: a reply only once asked.
                b'R' if digits(params) && self.asked.contains(&"dsr") => Some("dsr"),
                b'n' if params == b"0" || params == b"3" => Some("status"),
                b'c' if params.first() == Some(&b'?') => Some("da1"),
                b'c' if params.first() == Some(&b'>') => Some("da2"),
                _ => None,
            }
        } else if kind == "string" {
            if bytes.starts_with(b"\x1b]10;") {
                Some("osc10")
            } else if bytes.starts_with(b"\x1b]11;") {
                Some("osc11")
            } else if bytes.starts_with(b"\x1bP>|") {
                Some("xtversion")
            } else {
                None
            }
        } else {
            return (kind, None);
        };
        match query {
            Some(query) => match self.queries.iter().position(|waiting| *waiting == query) {
                Some(position) => {
                    self.queries.remove(position);
                    ("reply", Some(query))
                }
                None => ("unsolicited", Some(query)),
            },
            None => ("key", None),
        }
    }

    fn tokens(&mut self, tokens: Vec<Token>) {
        for token in tokens {
            self.token(token);
        }
    }

    fn token(&mut self, token: Token) {
        let (kind, query) = self.classify(token.kind, &token.bytes);
        let mut value = json!({
            "kind": if kind == "unsolicited" { "reply" } else { kind },
            "t": seconds(token.at),
            "read": token.read,
            "hex": hex(&token.bytes),
        });
        let content = if kind == "paste" {
            &token.bytes[PASTE_START.len()..token.bytes.len() - PASTE_END.len()]
        } else {
            &token.bytes[..]
        };
        match kind {
            "text" | "paste" | "key" | "reply" | "unsolicited" => {
                value["text"] = json!(lossy(content))
            }
            "control" => value["name"] = json!(control_name(token.bytes[0])),
            _ => {}
        }
        if kind == "reply" || kind == "unsolicited" {
            value["query"] = json!(query);
        }
        if kind == "unsolicited" {
            value["unsolicited"] = json!(true);
        }
        if let Some((at, read)) = token.began {
            value["t0"] = json!(seconds(at));
            value["read0"] = json!(read);
        }
        if token.incomplete {
            value["incomplete"] = json!(true);
        }
        record(value);
        match kind {
            "text" => {
                self.composer.push_str(&lossy(content));
                if self.framed() {
                    self.frame();
                } else if self.config.echo {
                    out(content);
                }
            }
            "paste" => {
                self.composer.push_str(&lossy(content));
                if self.framed() {
                    let text = self.composer.replace("\r\n", "\n").replace('\r', "\n");
                    self.composer = text;
                    self.frame();
                } else if self.config.echo {
                    let text = lossy(content).replace("\r\n", "\n").replace('\r', "\n");
                    out(text.replace('\n', "\r\n").as_bytes());
                }
            }
            "enter" if self.config.swallow_enter => {}
            "enter" => self.submit(),
            "ctrl-c" => {
                self.composer.clear();
                if self.framed() {
                    self.frame();
                }
            }
            // Ctrl+U deletes the current line's text, as the native composers
            // do; on an empty line it does nothing (zerus-wkj9l).
            "control" if token.bytes[0] == 0x15 => {
                let start = self.composer.rfind('\n').map_or(0, |index| index + 1);
                if start < self.composer.len() {
                    self.composer.truncate(start);
                    if self.framed() {
                        self.frame();
                    } else if self.config.echo {
                        out(b"\r\x1b[K");
                        if start == 0 {
                            out(&self.config.prompt);
                        }
                    }
                }
            }
            "control" if matches!(token.bytes[0], 0x7f | 0x08) && self.composer.pop().is_some() && self.config.echo => {
                if self.framed() {
                    self.frame();
                } else {
                    out(b"\x08 \x08");
                }
            }
            _ => {}
        }
        let keys = &self.config.exit_keys;
        if kind == "paste" {
            self.recent.clear();
        } else if !keys.is_empty() {
            self.recent.extend_from_slice(&token.bytes);
            let excess = self.recent.len().saturating_sub(keys.len());
            self.recent.drain(..excess);
            if self.recent == *keys && self.exit_now.is_none() {
                self.exit_now = Some((self.config.exit_code, "keys".into()));
            }
        }
    }

    /// Whether the composer is drawn as Claude Code's frame (`--claude-frame`).
    fn framed(&self) -> bool {
        self.config.claude_frame && self.config.echo
    }

    /// Claude Code's composer frame from the top of the screen: a rule, `❯ `
    /// and the composer's lines, a rule below, the cursor after the text.
    fn frame(&self) {
        let rule = "─".repeat(40);
        let lines: Vec<&str> = self.composer.split('\n').collect();
        let mut screen = format!("\x1b[H\x1b[J{rule}\r\n");
        for (n, line) in lines.iter().enumerate() {
            screen.push_str(if n == 0 { "❯ " } else { "  " });
            screen.push_str(line);
            screen.push_str("\r\n");
        }
        screen.push_str(&rule);
        let column = 3 + lines.last().map_or(0, |line| line.chars().count());
        screen.push_str(&format!("\x1b[{};{column}H", lines.len() + 1));
        out(screen.as_bytes());
    }

    fn submit(&mut self) {
        if self.framed() {
            let prompt = std::mem::take(&mut self.composer);
            self.frame();
            self.composer = prompt;
        } else if self.config.echo {
            out(b"\r\n");
            out(&self.config.prompt);
        }
        let prompt = std::mem::take(&mut self.composer);
        if prompt.is_empty() || !self.started {
            return;
        }
        self.hook("submit", Some(&prompt));
        // Claude Code writes the user row after its UserPromptSubmit hook.
        if self.claude_rows {
            if let Some(path) = &self.transcript {
                let row = json!({"type": "user", "message": {"role": "user", "content": prompt}});
                if let Err(error) = fs::OpenOptions::new().append(true).open(path).and_then(|mut file| {
                    std::io::Write::write_all(&mut file, format!("{row}\n").as_bytes())
                }) {
                    record(json!({"kind": "error", "transcript": path.to_string_lossy(), "error": error.to_string()}));
                }
            }
        }
        match self.config.turn {
            Some(turn) if turn.is_zero() => self.hook("stop", None),
            Some(turn) => self.stop_at = self.stop_at.or(Some(Instant::now() + turn)),
            None => {}
        }
    }

    fn hook(&mut self, event: &str, prompt: Option<&str>) {
        if !self.config.hook_events.contains(&event) {
            return;
        }
        let Some(&(_, name)) = EVENTS.iter().find(|(short, _)| *short == event) else {
            return;
        };
        let mut commands: Vec<Hook> = self
            .hooks
            .iter()
            .filter(|hook| hook.event == name)
            .cloned()
            .collect();
        commands.extend(self.config.hook_command.iter().map(|command| Hook {
            event: name.into(),
            command: command.clone(),
            args: None,
            runner: Runner::Direct,
            timeout: None,
        }));
        if commands.is_empty() {
            return;
        }
        let cwd = env::current_dir().unwrap_or_default();
        let mut payload = json!({
            "session_id": self.session_id,
            "hook_event_name": name,
            "cwd": cwd.to_string_lossy(),
        });
        if let Some(path) = &self.transcript {
            payload["transcript_path"] = json!(path.to_string_lossy());
        }
        match event {
            "start" => payload["source"] = json!(if self.resumed { "resume" } else { "startup" }),
            "submit" => payload["prompt"] = json!(prompt.unwrap_or_default()),
            "stop" => payload["stop_hook_active"] = json!(false),
            _ => payload["reason"] = json!(self.config.end_reason),
        }
        for (key, value) in &self.config.hook_extra {
            payload[key] = value.clone();
        }
        let payload = serde_json::to_vec(&payload).unwrap_or_default();
        for hook in commands {
            let mut fields = run_hook(
                &hook,
                &payload,
                hook.timeout.unwrap_or(self.config.hook_timeout),
            );
            let blocked = fields.get("status") == Some(&json!(2));
            fields.insert("kind".into(), json!("hook"));
            fields.insert("event".into(), json!(name));
            fields.insert("command".into(), json!(hook.command));
            if let Some(args) = &hook.args {
                fields.insert("args".into(), json!(args));
            }
            fields.insert("blocked".into(), json!(blocked));
            record(Value::Object(fields));
            if blocked {
                break;
            }
        }
    }

    fn apply_modes(&mut self, modes: &[(&'static str, bool)]) {
        let mut bytes = Vec::new();
        for (name, on) in modes {
            let Some((_, set, reset)) = MODES.iter().find(|(known, _, _)| known == name) else {
                continue;
            };
            bytes.extend_from_slice(if *on { set } else { reset });
            self.modes.retain(|active| active != name);
            if *on {
                self.modes.push(name);
            }
        }
        if !modes.is_empty() {
            out(&bytes);
            let names: Vec<String> = modes
                .iter()
                .map(|(name, on)| {
                    if *on {
                        (*name).to_owned()
                    } else {
                        format!("no-{name}")
                    }
                })
                .collect();
            record(json!({"kind": "modes", "modes": names, "active": self.modes}));
        }
    }

    fn send_queries(&mut self, queries: &[&'static str]) {
        let mut bytes = Vec::new();
        for query in queries {
            if let Some((_, sequence)) = QUERIES.iter().find(|(known, _)| known == query) {
                bytes.extend_from_slice(sequence);
                self.queries.push(query);
                self.asked.push(query);
            }
        }
        if !queries.is_empty() {
            out(&bytes);
            record(json!({"kind": "query", "queries": queries}));
        }
    }

    fn render(&self, path: &Path) {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                record(
                    json!({"kind": "error", "render": path.to_string_lossy(), "error": error.to_string()}),
                );
                return;
            }
        };
        let chunk = if self.config.render_chunk == 0 {
            bytes.len().max(1)
        } else {
            self.config.render_chunk
        };
        let mut steps = 0;
        for (index, part) in bytes.chunks(chunk).enumerate() {
            if index > 0 && !self.config.render_delay.is_zero() {
                thread::sleep(self.config.render_delay);
            }
            out(part);
            steps += 1;
        }
        record(
            json!({"kind": "render", "path": path.to_string_lossy(), "bytes": bytes.len(), "steps": steps}),
        );
    }

    fn poll_control(&mut self) {
        let Some(path) = self.config.control.clone() else {
            return;
        };
        let Ok(text) = fs::read_to_string(&path) else {
            return;
        };
        let _ = fs::remove_file(&path);
        for line in text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            let result = self.command(line);
            let mut value = json!({"kind": "control", "command": line});
            if let Err(error) = result {
                value["error"] = json!(error);
            }
            record(value);
        }
    }

    fn command(&mut self, line: &str) -> Result<(), String> {
        let (command, rest) = match line.split_once(char::is_whitespace) {
            Some((command, rest)) => (command, rest.trim()),
            None => (line, ""),
        };
        match command {
            "write" => out(&unescape(rest)?),
            "render" => self.render(Path::new(rest)),
            "steps" => {
                let mut parts = rest.split_whitespace();
                let chunk = number(parts.next().unwrap_or("0"))?;
                self.config.render_chunk = chunk as usize;
                self.config.render_delay = millis(parts.next().unwrap_or("0"))?;
            }
            "modes" => {
                let modes = parse_modes(rest)?;
                self.apply_modes(&modes);
            }
            "query" => {
                let queries = parse_queries(rest)?;
                self.send_queries(&queries);
            }
            "hook" => {
                let (event, prompt) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                if !EVENTS.iter().any(|(short, _)| *short == event) {
                    return Err(format!("unknown hook event {event:?}"));
                }
                self.hook(event, Some(prompt));
            }
            "exit" => {
                let code = if rest.is_empty() {
                    self.config.exit_code
                } else {
                    parse_code(rest)?
                };
                self.exit_now = Some((code, "control".into()));
            }
            _ => return Err(format!("unknown control command {command:?}")),
        }
        Ok(())
    }

    fn write_ready(&self, tty: bool, raw: bool) {
        let Some(path) = &self.config.ready else {
            return;
        };
        let value = json!({
            "pid": process::id(),
            "process_start": self.process_start,
            "cwd": self.cwd,
            "ready_unix_ms": unix_ms(),
            "name": self.config.name,
            "argv": self.config.argv,
            "session_id": self.session_id,
            "resumed": self.resumed,
            "tty": tty,
            "raw": raw,
            "size": self.size,
            "transcript_path": self.transcript.as_ref().map(|path| path.to_string_lossy()),
        });
        let mut name = OsString::from(".");
        name.push(path.file_name().unwrap_or_default());
        name.push(format!(".{}.tmp", process::id()));
        let temporary = path.with_file_name(name);
        let mut bytes = serde_json::to_vec(&value).unwrap_or_default();
        bytes.push(b'\n');
        match fs::write(&temporary, &bytes).and_then(|()| fs::rename(&temporary, path)) {
            Ok(()) => record(json!({"kind": "ready", "path": path.to_string_lossy()})),
            Err(error) => record(
                json!({"kind": "error", "ready": path.to_string_lossy(), "error": error.to_string()}),
            ),
        }
    }

    fn finish(&mut self, code: i32, cause: &str) -> ! {
        let mut tokens = Vec::new();
        self.tokenizer.flush(&mut tokens);
        self.tokens(tokens);
        if let Some((content, at, read)) = self.tokenizer.paste.take() {
            let mut bytes = PASTE_START.to_vec();
            bytes.extend_from_slice(&content);
            record(
                json!({"kind": "paste", "t": seconds(at), "read0": read, "read": self.reads,
                          "hex": hex(&bytes), "text": lossy(&content), "incomplete": true}),
            );
        }
        self.hook("end", None);
        if self.config.reset_modes {
            let mut bytes = Vec::new();
            for name in self.modes.iter().rev() {
                if let Some((_, _, reset)) = MODES.iter().find(|(known, _, _)| known == name) {
                    bytes.extend_from_slice(reset);
                }
            }
            out(&bytes);
        }
        platform::restore();
        record(json!({"kind": "exit", "code": code, "cause": cause}));
        process::exit(code)
    }
}

fn main() {
    START.get_or_init(Instant::now);
    let started_unix_ms = unix_ms();
    let config = Config::load().unwrap_or_else(|error| {
        eprintln!("fake_agent: {error}");
        process::exit(2);
    });
    if let Some(text) = &config.probe_text {
        if config.argv == ["--version"] || config.argv == ["--help"] {
            println!("{text}");
            process::exit(0);
        }
    }
    if let Some(path) = &config.record {
        match OpenOptions::new().create(true).append(true).open(path) {
            Ok(file) => {
                *RECORD
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(file)
            }
            Err(error) => {
                eprintln!("fake_agent: {}: {error}", path.display());
                process::exit(2);
            }
        }
    }
    let (sender, events) = mpsc::channel();
    let _ = EVENTS_TX.set(sender);
    let _ = POLICIES.set(config.policies.clone());
    platform::install_signals(policy_for);
    let tty = io::stdin().is_terminal();
    platform::prepare_output();
    let raw = config.raw && platform::make_raw();
    let (session_id, resumed) = match (&config.session_id, agent_session(&config.argv)) {
        (Some(id), (_, resumed)) => (id.clone(), resumed),
        (None, (Some(id), resumed)) => (id, resumed),
        (None, (None, _)) => (random_uuid(), false),
    };
    let transcript = config.transcript_dir.as_ref().map(|directory| {
        let path = directory.join(format!("{session_id}.jsonl"));
        let line = format!("{}\n", json!({"session_id": session_id}));
        if let Err(error) = fs::create_dir_all(directory).and_then(|()| {
            if path.exists() {
                Ok(())
            } else {
                fs::write(&path, line)
            }
        }) {
            record(json!({"kind": "error", "transcript": path.to_string_lossy(), "error": error.to_string()}));
        }
        path
    });
    let style = config.hooks.clone().unwrap_or_else(|| {
        ["claude", "codex", "kimi"]
            .contains(&config.name.as_str())
            .then(|| config.name.clone())
    });
    let claude_rows = style.as_deref() == Some("claude") && transcript.is_some();
    let (hooks, info) = match style {
        Some(agent) => {
            let (hooks, info) = load_hooks(&agent);
            (hooks, Some(info))
        }
        None => (Vec::new(), None),
    };
    let size = platform::size();
    let process_start = platform::process_start();
    let cwd = env::current_dir()
        .ok()
        .map(|path| path.to_string_lossy().into_owned());
    record(json!({
        "kind": "start",
        "t": 0.0,
        "pid": process::id(),
        "process_start": process_start,
        "cwd": cwd,
        "unix_ms": started_unix_ms,
        "name": config.name,
        "argv": config.argv,
        "session_id": session_id,
        "resumed": resumed,
        "tty": tty,
        "raw": raw,
        "size": size,
    }));
    if let Some(info) = info {
        record(info);
    }
    thread::spawn(platform::read_input);
    let mut agent = Agent {
        tokenizer: Tokenizer::default(),
        events,
        reads: 0,
        queries: Vec::new(),
        asked: Vec::new(),
        composer: String::new(),
        modes: Vec::new(),
        hooks,
        session_id,
        resumed,
        transcript,
        claude_rows,
        stop_at: None,
        exit_at: None,
        exit_now: None,
        recent: Vec::new(),
        started: false,
        size,
        process_start,
        cwd,
        config,
    };
    let modes = agent.config.modes.clone();
    agent.apply_modes(&modes);
    if let Some(path) = agent.config.render.clone() {
        agent.render(&path);
    }
    if agent.framed() {
        agent.frame();
    } else {
        out(&agent.config.prompt);
    }
    let queries = agent.config.queries.clone();
    agent.send_queries(&queries);
    if !queries.is_empty() && !agent.config.query_wait.is_zero() {
        let deadline = Instant::now() + agent.config.query_wait;
        agent.run_until(Some(deadline), |agent| agent.queries.is_empty());
        if !agent.queries.is_empty() {
            record(json!({"kind": "query-timeout", "pending": agent.queries}));
            if let Some(code) = agent.config.query_fail_code {
                agent.finish(code, "query-timeout");
            }
        }
    }
    agent.started = true;
    agent.hook("start", None);
    agent.write_ready(tty, raw);
    agent.run_until(None, |_| false);
}
