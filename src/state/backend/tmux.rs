//! The tmux backend. Every call spawns plain `tmux` by name, so a PATH shim and an
//! inherited `TMUX` or `TMUX_TMPDIR` select the server; no call passes `-L`, `-S`
//! or `-f`. Exact names are `=NAME` for session commands and `=NAME:` (the
//! session's pane) for pane and option commands. On Linux a `new-session` that
//! may start the server runs in a scope of its own when hgs runs in a user unit
//! (`scope`); it still spawns that same `tmux` command.
use super::{
    ClientExit, ClientRow, DryRun, InputClaim, InputState, Key, LaunchSpec, Launched, Listing,
    PaneField, PaneInfo, PanePath, PaneRow, Paste, SessionRow, StageErrors, Target,
    TerminalBackend, TerminalState,
};
use crate::cli::{Error, Result as CliResult};
use crate::platform;
use crate::state::{nonempty_env, Result};
use std::fs;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::process::{Command, Output, Stdio};

#[cfg(target_os = "linux")]
mod scope;

pub(crate) struct Tmux;

/// `tmux ARGS` with captured output. With `check`, a failure becomes tmux's own
/// message, or "tmux command failed" when it printed none.
fn tmux(args: &[String], check: bool) -> Result<Output> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if crate::state::effort::picker_entered() {
        return crate::state::effort::picker_refuse(
            "Unowned tmux call refused in original Effort keeper".into(),
        );
    }
    checked(Command::new("tmux").args(args).output(), check)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn picker(call: crate::state::effort::PickerRead, args: &[String], check: bool) -> Result<Output> {
    if let Some(output) = crate::state::effort::picker_helper_read(call, args) {
        return output
            .and_then(|output| checked(Ok(output), check))
            .or_else(crate::state::effort::picker_refuse);
    }
    tmux(args, check)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn picker_ps(args: &[String]) -> Result<Output> {
    if let Some(output) =
        crate::state::effort::picker_helper_read(crate::state::effort::PickerRead::ProcessTty, args)
    {
        return output;
    }
    Command::new("ps")
        .args(args)
        .output()
        .map_err(|e| e.to_string())
}

/// Run tmux `args`, a command that may start the server, with `run`. Where that
/// server would belong to the user unit hgs runs in, the command runs in a scope
/// of its own (`scope`). When systemd-run could not make the scope, tmux never
/// ran, and the command runs as it always has unless `rerun` refuses the result.
fn starting<T>(
    args: &[String],
    run: impl Fn(&mut Command) -> T,
    rerun: impl FnOnce(&T) -> bool,
) -> T {
    #[cfg(target_os = "linux")]
    if let Some(scope) = scope::Scope::new(|| Tmux.server_running()) {
        let result = run(without_session_context(&mut scope.command(args)));
        if scope.started() || !rerun(&result) {
            return result;
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = rerun;
    run(without_session_context(Command::new("tmux").args(args)))
}

/// A server keeps the environment of the client that started it as the
/// global environment of every pane, so a command that may start one leaves
/// out the session and agent it runs in (zerus-01sz8): a launch sets again
/// what its own session needs. `TMUX` stays, as it selects the server. A
/// running server takes none of these from a `new-session` client.
fn without_session_context(command: &mut Command) -> &mut Command {
    for name in super::host::SESSION_CONTEXT
        .iter()
        .chain(super::AGENT_COMMAND_CONTEXT)
        .filter(|name| **name != "TMUX")
    {
        command.env_remove(name);
    }
    command
}

fn checked(output: io::Result<Output>, check: bool) -> Result<Output> {
    let output = output.map_err(|e| format!("tmux: {e}"))?;
    if check && !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if error.is_empty() {
            "tmux command failed".into()
        } else {
            error
        });
    }
    Ok(output)
}

fn owned(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

/// Multiple arguments use tmux's direct exec path, even for one program. Its
/// command parser still consumes a final semicolon unless escaped once here.
fn direct_command(mut arguments: Vec<String>, command: &[String]) -> Vec<String> {
    arguments.extend(owned(&["--", "/usr/bin/env", "--"]));
    arguments.extend(command.iter().cloned());
    for argument in &mut arguments {
        if argument.ends_with(';') {
            argument.insert(argument.len() - 1, '\\');
        }
    }
    arguments
}

/// The `-t` operand of a session command.
fn session_target(target: Target) -> String {
    match target {
        Target::Name(name) => format!("={name}"),
        Target::Session(id) | Target::Pane(id) => id.to_owned(),
    }
}

/// The `-t` operand of a pane or option command.
fn pane_target(target: Target) -> String {
    match target {
        Target::Name(name) => format!("={name}:"),
        Target::Session(id) | Target::Pane(id) => id.to_owned(),
    }
}

fn field_format(field: PaneField) -> &'static str {
    match field {
        PaneField::SessionId => "#{session_id}",
        PaneField::SessionName => "#{session_name}",
        PaneField::SocketPath => "#{socket_path}",
        PaneField::SessionCreated => "#{session_created}",
        PaneField::SessionWindows => "#{session_windows}",
        PaneField::SessionGrouped => "#{session_grouped}",
        PaneField::WindowPanes => "#{window_panes}",
        PaneField::WindowLinked => "#{window_linked}",
        PaneField::PaneId => "#{pane_id}",
        PaneField::ServerPid => "#{pid}",
        PaneField::PanePid => "#{pane_pid}",
        PaneField::Width => "#{pane_width}",
        PaneField::Height => "#{pane_height}",
        PaneField::CursorX => "#{cursor_x}",
        PaneField::CursorY => "#{cursor_y}",
        PaneField::InMode => "#{pane_in_mode}",
        PaneField::CurrentCommand => "#{pane_current_command}",
        PaneField::Dead => "#{pane_dead}",
        PaneField::RunMarker => "#{@hgs_run}",
        PaneField::BracketedPaste => "#{bracket_paste_flag}",
        PaneField::Tty => "#{pane_tty}",
    }
}

/// The `display-message` format for `fields`, tab separated.
fn format(fields: &[PaneField]) -> String {
    fields
        .iter()
        .map(|field| field_format(*field))
        .collect::<Vec<_>>()
        .join("\t")
}

/// What `new-session -P` prints of the terminal it created: the server's pid
/// and start time, the pane, and the pane's process (zerus-5sl7r). The
/// fields are separated by a colon, which none of them holds: a client whose
/// locale is not UTF-8 gets a tab as `_`.
const LAUNCHED: &str = "#{pid}:#{start_time}:#{pane_id}:#{pane_pid}";
/// What one observation reads of a pane `-t %N` names: the answering server's
/// pid and start time, the pane it resolved (empty when it has none: tmux
/// then still answers, with no pane), whether its command ended, and its
/// process (empty once it ended).
const OBSERVED: &str = "#{pid}:#{start_time}:#{pane_id}:#{pane_dead}:#{pane_pid}";

/// The terminal `new-session -P -F LAUNCHED` reports, when it reports one;
/// the launched process's birth is read separately ([`child_birth`]).
fn launched(stdout: &[u8]) -> Option<Launched> {
    let text = String::from_utf8_lossy(stdout);
    let mut fields = text.trim_end_matches(['\n', '\r']).split(':');
    match (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    ) {
        (Some(pid), Some(start), Some(pane), Some(process), None)
            if !pid.is_empty() && !start.is_empty() && pane.starts_with('%') =>
        {
            Some(Launched {
                server: format!("{pid}:{start}"),
                pane: pane.into(),
                process: process.parse().ok().filter(|&process| process > 0)?,
                birth: None,
            })
        }
        _ => None,
    }
}

/// The kernel birth of `process` when it is a child of the tmux server
/// `server` (a pane's process is the server's child), else None.
fn child_birth(process: u32, server: &str) -> Option<String> {
    let server: u32 = server.split(':').next()?.parse().ok()?;
    platform::process_identity(process)
        .filter(|identity| identity.parent == server)
        .map(|identity| identity.birth)
}

/// What tmux's answer to `display-message -p -t %N OBSERVED` proves of the
/// terminal `launched` names (zerus-5sl7r); `birth` reads a process's kernel
/// birth now. Only the server that created the pane (same pid and start time)
/// says anything: a server at that socket path with another identity (one
/// started there while the original runs on), a refused or missing socket, a
/// failed program or a signal proves nothing. That server answers a pane it
/// has no longer with an empty pane, and never gives its id to another pane
/// while it runs. A live pane is the launch only while it runs the launched
/// process (same pid and kernel birth); another process there (`respawn-pane`)
/// is [`TerminalState::Replaced`], and one whose birth cannot be read proves
/// nothing.
fn observed_state(
    launched: &Launched,
    status: Option<i32>,
    stdout: &[u8],
    birth: impl Fn(u32) -> Option<String>,
) -> TerminalState {
    if status != Some(0) {
        return TerminalState::Unknown;
    }
    let text = String::from_utf8_lossy(stdout);
    let mut fields = text.trim_end_matches(['\n', '\r']).split(':');
    let (Some(pid), Some(start), Some(pane), Some(dead), Some(process), None) = (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    ) else {
        return TerminalState::Unknown;
    };
    if pid.is_empty() || format!("{pid}:{start}") != launched.server {
        return TerminalState::Unknown;
    }
    if pane != launched.pane {
        return TerminalState::Absent;
    }
    match dead {
        "1" => TerminalState::DeadPane,
        "0" if process != launched.process.to_string() => TerminalState::Replaced,
        "0" => match (birth(launched.process), &launched.birth) {
            (Some(now), Some(then)) if &now == then => TerminalState::Alive,
            (Some(_), Some(_)) => TerminalState::Replaced,
            _ => TerminalState::Unknown,
        },
        _ => TerminalState::Unknown,
    }
}

pub(in crate::state) fn pane_info_args(target: Target, fields: &[PaneField]) -> Vec<String> {
    vec![
        "display-message".into(),
        "-p".into(),
        "-t".into(),
        pane_target(target),
        format(fields),
    ]
}

/// Commands that send `keys`: one per run of named keys or of literal text, and
/// one without keys for an empty list.
fn send_args(pane: &str, keys: &[Key]) -> Vec<Vec<String>> {
    let base = || owned(&["send-keys", "-t", pane]);
    let mut commands: Vec<(bool, Vec<String>)> = Vec::new();
    for key in keys {
        let (literal, text) = match key {
            Key::Named(name) => (false, *name),
            Key::Literal(text) => (true, *text),
        };
        match commands.last_mut() {
            Some((kind, arguments)) if *kind == literal => arguments.push(text.into()),
            _ => {
                let mut arguments = base();
                if literal {
                    arguments.push("-l".into());
                }
                arguments.push(text.into());
                commands.push((literal, arguments));
            }
        }
    }
    if commands.is_empty() {
        return vec![base()];
    }
    commands
        .into_iter()
        .map(|(_, arguments)| arguments)
        .collect()
}

fn paste_args(buffer: &str, pane: &str, mode: Paste) -> Vec<String> {
    match mode {
        Paste::Composer => owned(&["paste-buffer", "-p", "-r", "-d", "-b", buffer, "-t", pane]),
        Paste::Terminal { bracketed } => {
            let mut arguments = owned(&["paste-buffer", "-r", "-d", "-b", buffer, "-t", pane]);
            if bracketed {
                arguments.push("-p".into());
            }
            arguments
        }
    }
}

/// A detached relaunch: `new-session -d -s NAME -c DIR (-e KEY=VALUE)* COMMAND`.
fn start_args(spec: &LaunchSpec) -> Vec<String> {
    let mut arguments = owned(&["new-session", "-d", "-s", &spec.name, "-c", &spec.cwd]);
    for value in &spec.env {
        arguments.extend(["-e".into(), value.clone()]);
    }
    direct_command(arguments, &spec.command)
}

/// [`start_args`] that also print the terminal created ([`LAUNCHED`]).
fn start_observed_args(spec: &LaunchSpec) -> Vec<String> {
    let mut arguments = start_args(spec);
    arguments.splice(2..2, owned(&["-P", "-F", LAUNCHED]));
    arguments
}

/// A CLI launch: `-u new-session [-d] (-e KEY=VALUE)* -s NAME -c DIR COMMAND`.
fn launch_args(spec: &LaunchSpec) -> Vec<String> {
    let mut arguments = owned(&["-u", "new-session"]);
    if spec.detached {
        arguments.push("-d".into());
    }
    for value in &spec.env {
        arguments.extend(["-e".into(), value.clone()]);
    }
    arguments.extend([
        "-s".into(),
        spec.name.clone(),
        "-c".into(),
        spec.cwd.clone(),
    ]);
    direct_command(arguments, &spec.command)
}

/// Foreground programs that mean a shell, not the agent, reads the pane's input.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "fish", "dash", "tcsh"];

impl Tmux {
    /// After a client exits with an error, a dead server leaves the terminal in
    /// whatever state the client had set.
    fn client_done(&self, rc: i32) -> bool {
        if rc != 0 && !self.server_running() {
            platform::restore_terminal();
            eprintln!(
                "hgs: the tmux server died; terminal restored, sessions on this box are gone"
            );
            true
        } else {
            false
        }
    }
}

/// The unchanged native input check with explicit fixed read callbacks.
/// A scoped owner can retain its exact helper Children without changing the
/// process-wide backend or installing a persistent routing flag.
pub(in crate::state) fn input_state_using(
    claim: &InputClaim,
    pane_info: impl FnOnce(&[PaneField]) -> Result<PaneInfo>,
    process: impl FnOnce(&[String]) -> Result<Output>,
) -> Result<InputState> {
    let info = pane_info(
        &[
            PaneField::Tty,
            PaneField::CursorX,
            PaneField::CursorY,
            PaneField::InMode,
            PaneField::CurrentCommand,
            PaneField::Dead,
            PaneField::RunMarker,
            PaneField::SessionName,
            PaneField::BracketedPaste,
        ],
    )?;
    let fields = info.fields();
    if fields.len() != 9
        || fields[3] != "0"
        || fields[5] != "0"
        || fields[6] != claim.run_id
        || fields[7] != claim.name
        || fields[8] != "1"
    {
        return Err("terminal is not in the expected agent input mode; open Terminal".into());
    }
    let tty = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(fields[0])
        .map_err(|e| e.to_string())?;
    let pid = claim.pid.ok_or("missing agent process")? as i32;
    let mut attributes: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(tty.as_raw_fd(), &mut attributes) } != 0
        || attributes.c_lflag & (libc::ICANON | libc::ECHO) != 0
    {
        return Err("agent does not own an interactive input terminal; open Terminal".into());
    }
    // Ensure the pinned process is attached to this tty, not just a PID sharing a group.
    let tty_output = process(&owned(&[
        "-p",
        &pid.to_string(),
        "-o",
        "tty=,pgid=,tpgid=,comm=",
    ]))?;
    let tty_text = String::from_utf8_lossy(&tty_output.stdout);
    let process_fields: Vec<_> = tty_text.split_whitespace().collect();
    if process_fields.len() < 4
        || process_fields[0] != fields[0].trim_start_matches("/dev/")
        || process_fields[1] != process_fields[2]
        || !process_fields[2].parse::<i32>().is_ok_and(|id| id > 0)
    {
        return Err("tracked agent is not attached to the selected terminal".into());
    }
    if SHELLS.contains(&fields[4])
        && !(claim.agent == "kimi"
            && Path::new(process_fields[3])
                .file_name()
                .is_some_and(|name| name == "kimi"))
    {
        return Err("terminal is not in the expected agent input mode; open Terminal".into());
    }
    Ok(InputState {
        cursor_x: fields[1].into(),
        cursor_y: fields[2].into(),
    })

}

impl TerminalBackend for Tmux {
    fn server_running(&self) -> bool {
        platform::silent("tmux", &["list-sessions"])
    }
    fn inside(&self) -> bool {
        std::env::var("TMUX").is_ok_and(|s| !s.is_empty())
    }
    fn enclosing_session(&self) -> String {
        platform::capture("tmux", &["display", "-p", "#S"])
            .unwrap_or_default()
            .trim()
            .to_owned()
    }
    fn own_pane(&self) -> Result<String> {
        nonempty_env("TMUX_PANE").ok_or_else(|| "missing TMUX_PANE in tracked agent launch".into())
    }

    fn listing(&self) -> Result<Listing> {
        let clients = platform::capture(
            "tmux",
            &["list-clients", "-F", "#{client_session}\t#{client_tty}"],
        )
        .unwrap_or_default();
        let rows = platform::capture(
            "tmux",
            &[
                "list-sessions",
                "-F",
                "#{session_name}\t#{session_attached}\t#{session_created}\t#{session_id}\t#{HGS_LAUNCH_ID}",
            ],
        )
        .unwrap_or_default();
        let clients = clients
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .map(|(session, tty)| ClientRow {
                session: session.into(),
                terminal: tty.into(),
            })
            .collect();
        let sessions = rows
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                let mut fields = line.split('\t');
                SessionRow {
                    name: fields.next().unwrap_or_default().into(),
                    attached: fields.next().unwrap_or("0").parse::<u64>().unwrap_or(0),
                    created: fields.next().unwrap_or("0").parse::<u64>().unwrap_or(0),
                    id: fields.next().unwrap_or_default().into(),
                    launch_id: fields.next().unwrap_or_default().into(),
                }
            })
            .collect();
        // tmux reports a failed listing as no sessions, as hgs always has.
        Ok(Listing { clients, sessions })
    }
    fn last_client_input(&self, name: &str) -> Option<Option<f64>> {
        let target = format!("={name}");
        let output = tmux(
            &owned(&["list-clients", "-t", &target, "-F", "#{client_activity}"]),
            false,
        )
        .ok()?;
        if !output.status.success() {
            return None;
        }
        let mut latest = None;
        for row in String::from_utf8_lossy(&output.stdout).lines() {
            let at = row.trim().parse::<f64>().ok()?;
            latest = Some(latest.map_or(at, |seen: f64| seen.max(at)));
        }
        Some(latest)
    }
    fn attached_counts(&self) -> Result<Option<Vec<(String, String)>>> {
        let Some(rows) = platform::capture(
            "tmux",
            &["list-sessions", "-F", "#{session_name}|#{session_attached}"],
        )
        .filter(|rows| !rows.trim().is_empty()) else {
            return Ok(None);
        };
        Ok(Some(
            rows.lines()
                .filter_map(|row| row.split_once('|'))
                .map(|(name, count)| (name.into(), count.into()))
                .collect(),
        ))
    }
    fn panes(&self) -> Result<Vec<PaneRow>> {
        let output = picker(
            crate::state::effort::PickerRead::Panes,
            &owned(&[
                "list-panes",
                "-a",
                "-F",
                "#{session_name}\t#{session_id}\t#{pane_id}\t#{pane_dead}\t#{@hgs_run}",
            ]),
            false,
        )?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let fields: Vec<&str> = line.splitn(5, '\t').collect();
                (fields.len() == 5).then(|| PaneRow {
                    session: fields[0].into(),
                    session_id: fields[1].into(),
                    pane: fields[2].into(),
                    dead: fields[3].into(),
                    run_marker: fields[4].into(),
                })
            })
            .collect())
    }
    fn pane_paths(&self) -> Result<Vec<PanePath>> {
        let output = tmux(
            &owned(&[
                "list-panes",
                "-a",
                "-F",
                "#{session_name}\t#{session_id}\t#{window_active}\t#{pane_active}\t#{pane_current_path}",
            ]),
            false,
        )?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let parts: Vec<_> = line.splitn(5, '\t').collect();
                (parts.len() == 5).then(|| PanePath {
                    session: parts[0].into(),
                    session_id: parts[1].into(),
                    active_window: parts[2] == "1",
                    active_pane: parts[3] == "1",
                    path: parts[4].into(),
                })
            })
            .collect())
    }

    fn session_exists(&self, name: &str) -> bool {
        platform::silent("tmux", &["has-session", "-t", &format!("={name}")])
    }
    /// One `display-message` of the launched pane, bounded by `wait` and read
    /// in the C locale; a late observation is killed and unknown
    /// ([`observed_state`]).
    fn terminal_state(&self, launched: &Launched, wait: std::time::Duration) -> TerminalState {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if crate::state::effort::picker_entered() {
            return TerminalState::Unknown;
        }
        let arguments = owned(&["display-message", "-p", "-t", &launched.pane, OBSERVED]);
        match platform::output_until(
            Command::new("tmux").args(&arguments).env("LC_ALL", "C"),
            wait,
        ) {
            Ok(answer) => observed_state(launched, answer.status.code(), &answer.stdout, |pid| {
                platform::process_identity(pid).map(|identity| identity.birth)
            }),
            Err(_) => TerminalState::Unknown,
        }
    }
    fn session_name(&self, id: &str) -> Result<Option<String>> {
        let output = picker(
            crate::state::effort::PickerRead::SessionName,
            &owned(&["display-message", "-p", "-t", id, "#{session_name}"]),
            false,
        )?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
    }
    fn pane_info(&self, target: Target, fields: &[PaneField]) -> Result<PaneInfo> {
        let output = picker(
            crate::state::effort::PickerRead::PaneInfo,
            &pane_info_args(target, fields),
            true,
        )?;
        Ok(PaneInfo::new(
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
    }
    fn probe(&self, target: Target, fields: &[PaneField]) -> Option<PaneInfo> {
        let arguments = pane_info_args(target, fields);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if crate::state::effort::picker_entered() {
            let output = picker(crate::state::effort::PickerRead::Probe, &arguments, false).ok()?;
            return output
                .status
                .success()
                .then(|| PaneInfo::new(String::from_utf8_lossy(&output.stdout).trim().into()));
        }
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
        platform::capture("tmux", &arguments).map(PaneInfo::new)
    }
    fn start(&self, spec: &LaunchSpec) -> Result<()> {
        if spec.command.is_empty() {
            return Err("Session launch needs a program".into());
        }
        // Fork and relaunch submit a runner recipe naming this image; a unit
        // test image must not hand itself to tmux with production arguments.
        crate::self_exec::ensure().map_err(|e| e.to_string())?;
        let output = starting(&start_args(spec), Command::output, |_| true);
        checked(output, true).map(drop)
    }
    fn start_observed(&self, spec: &LaunchSpec) -> Result<Option<Launched>> {
        if spec.command.is_empty() {
            return Err("Session launch needs a program".into());
        }
        crate::self_exec::ensure().map_err(|e| e.to_string())?;
        let output = starting(&start_observed_args(spec), Command::output, |_| true);
        // The launched process's birth, read now: a process that already
        // ended, or is no child of that server, leaves it unknown.
        checked(output, true).map(|output| {
            launched(&output.stdout).map(|mut launched| {
                launched.birth = child_birth(launched.process, &launched.server);
                launched
            })
        })
    }
    fn set_run_marker(&self, name: &str, run_id: &str) -> Result<()> {
        tmux(
            &owned(&["set-option", "-t", &format!("={name}:"), "@hgs_run", run_id]),
            true,
        )
        .map(drop)
    }
    fn rename(&self, id: &str, name: &str) -> Result<()> {
        tmux(&owned(&["rename-session", "-t", id, name]), true).map(drop)
    }
    fn kill_session(&self, target: Target) -> Result<()> {
        tmux(
            &owned(&["kill-session", "-t", &session_target(target)]),
            true,
        )
        .map(drop)
    }
    fn kill_pane(&self, pane: &str) -> Result<()> {
        tmux(&owned(&["kill-pane", "-t", pane]), true).map(drop)
    }

    fn input_state(&self, claim: &InputClaim) -> Result<InputState> {
        input_state_using(claim, |fields| self.pane_info(Target::Pane(claim.pane), fields), picker_ps)
    }
    fn on_terminal(&self, pid: &str, tty: &str) -> Result<bool> {
        let output = picker_ps(&owned(&["-p", pid, "-o", "tty="]))?;
        Ok(output.status.success()
            && !tty.is_empty()
            && String::from_utf8_lossy(&output.stdout).trim() == tty.trim_start_matches("/dev/"))
    }
    fn capture(&self, pane: &str, mode: super::Capture) -> Result<Vec<u8>> {
        let arguments = match mode {
            super::Capture::Plain => owned(&["capture-pane", "-p", "-t", pane]),
            super::Capture::Styled => owned(&["capture-pane", "-p", "-e", "-t", pane]),
        };
        Ok(picker(
            match mode {
                super::Capture::Plain => crate::state::effort::PickerRead::CapturePlain,
                super::Capture::Styled => crate::state::effort::PickerRead::CaptureStyled,
            },
            &arguments,
            true,
        )?
        .stdout)
    }
    fn grow_unviewed(&self, pane: &str, rows: u16) -> Result<bool> {
        let size = tmux(
            &owned(&[
                "display-message",
                "-p",
                "-t",
                pane,
                "#{session_attached}\t#{window_height}",
            ]),
            true,
        )?;
        let size = String::from_utf8_lossy(&size.stdout);
        let height = match size.trim_end().split_once('\t') {
            Some(("0", height)) => height.parse::<u16>().ok(),
            _ => None,
        };
        let local = tmux(
            &owned(&["show-options", "-w", "-v", "-t", pane, "window-size"]),
            true,
        )?;
        if height.is_none_or(|height| height >= rows) || !local.stdout.trim_ascii().is_empty() {
            return Ok(false);
        }
        // resize-window sets the window's own window-size to manual; removing
        // it again lets a client that attaches later size the window.
        let resized = tmux(
            &owned(&["resize-window", "-t", pane, "-y", &rows.to_string()]),
            true,
        );
        tmux(
            &owned(&["set-option", "-w", "-u", "-t", pane, "window-size"]),
            true,
        )?;
        resized.map(|_| true)
    }
    fn send(&self, pane: &str, keys: &[Key]) -> Result<()> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if crate::state::effort::picker_entered() && keys.len() != 1 {
            return crate::state::effort::picker_refuse(
                "Effort key batch is not a single trusted choice".into(),
            );
        }
        for arguments in send_args(pane, keys) {
            picker(crate::state::effort::PickerRead::Keys, &arguments, true)?;
        }
        Ok(())
    }
    fn stage(&self, buffer: &str, text: &[u8], errors: StageErrors) -> Result<bool> {
        let mut loader = zerus_client_core::child_environment::spawn(Command::new("tmux")
            .args(["load-buffer", "-b", buffer, "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(match errors {
                StageErrors::Read => Stdio::piped(),
                StageErrors::Discard => Stdio::null(),
            }))
            .map_err(|e| e.to_string())?;
        let written = match loader.stdin.take() {
            Some(mut stdin) => stdin.write_all(text),
            None => Err(io::Error::other("missing buffer input")),
        };
        let status = match errors {
            StageErrors::Read => loader.wait_with_output().map_err(|e| e.to_string())?.status,
            StageErrors::Discard => loader.wait().map_err(|e| e.to_string())?,
        };
        Ok(status.success() && written.is_ok())
    }
    fn paste(&self, buffer: &str, pane: &str, mode: Paste) -> Result<()> {
        tmux(&paste_args(buffer, pane, mode), true).map(drop)
    }
    fn discard(&self, buffer: &str) {
        let _ = tmux(&owned(&["delete-buffer", "-b", buffer]), false);
    }

    fn launch(&self, spec: &LaunchSpec) -> CliResult<ClientExit> {
        if spec.command.is_empty() {
            return Err(Error::new(1, "Session launch needs a program"));
        }
        crate::self_exec::ensure().map_err(|e| Error::new(1, e.to_string()))?;
        let arguments = launch_args(spec);
        if spec.detached {
            let code = platform::code(starting(&arguments, Command::status, |_| true)?);
            return Ok(ClientExit {
                code,
                server_died: false,
            });
        }
        // A signal while systemd-run makes the scope cancels the launch.
        let code = starting(
            &arguments,
            platform::wait_interactive,
            |result| !matches!(result, Ok(code) if *code > 128),
        )?;
        Ok(ClientExit {
            code,
            server_died: self.client_done(code),
        })
    }
    fn set_client(&self, target: Target, client: &str) {
        platform::silent(
            "tmux",
            &[
                "set-environment",
                "-t",
                &session_target(target),
                "HGS_CLIENT",
                client,
            ],
        );
    }
    /// tmux attaches by the session ID the caller resolved after checking the
    /// run; it has no run check of its own.
    fn attach(&self, target: Target, _run_id: Option<&str>) -> CliResult<ClientExit> {
        let code = platform::wait_interactive(Command::new("tmux").args([
            "-u",
            "attach-session",
            "-t",
            &session_target(target),
        ]))?;
        Ok(ClientExit {
            code,
            server_died: self.client_done(code),
        })
    }
    fn kill(&self, name: &str) -> CliResult<i32> {
        Ok(platform::code(
            Command::new("tmux")
                .args(["kill-session", "-t", &format!("={name}")])
                .status()?,
        ))
    }
    /// Title output goes to the attached client terminals, never to an agent's pane
    /// input. SSH relays the same OSC sequence to clients on another machine.
    fn refresh_titles(&self, session: &str) -> CliResult<()> {
        if session.chars().any(char::is_control) {
            return Err(Error::new(1, "terminal title contains control characters"));
        }
        let target = format!("={session}");
        if !platform::silent("tmux", &["has-session", "-t", &target]) {
            return Ok(()); // Saved sessions have no attached terminals.
        }
        let option_target = format!("={session}:");
        for (key, value) in [("set-titles-string", "#S"), ("set-titles", "on")] {
            if !platform::silent("tmux", &["set-option", "-t", &option_target, key, value]) {
                return Err(Error::new(1, format!("cannot update tmux {key}")));
            }
        }
        let terminals = platform::capture(
            "tmux",
            &["list-clients", "-t", &target, "-F", "#{client_tty}"],
        )
        .ok_or_else(|| Error::new(1, "cannot list attached terminals"))?;
        // Konsole's previous hgs attachment may use a literal tab format. %w makes
        // it follow the window title; OSC 0 is also understood by macOS terminals.
        let title = format!("\x1b]30;%w\x07\x1b]0;{session}\x07");
        let mut seen = std::collections::BTreeSet::new();
        for path in terminals.lines().filter(|path| !path.is_empty()) {
            if !seen.insert(path) {
                continue;
            }
            if !path.starts_with("/dev/") || path.chars().any(char::is_control) {
                return Err(Error::new(1, "tmux returned an invalid client terminal"));
            }
            let mut terminal = fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_NOFOLLOW)
                .open(path)?;
            let metadata = terminal.metadata()?;
            if !metadata.file_type().is_char_device() || metadata.uid() != unsafe { libc::getuid() }
            {
                return Err(Error::new(1, "client terminal is not owned by this user"));
            }
            terminal.write_all(title.as_bytes())?;
        }
        Ok(())
    }
    fn viewer(&self, name: &str) -> String {
        let environment = platform::capture(
            "tmux",
            &["show-environment", "-t", &format!("={name}"), "HGS_CLIENT"],
        )
        .unwrap_or_default();
        let line = environment.trim_end_matches('\n');
        line.strip_prefix("HGS_CLIENT=").unwrap_or(line).to_owned()
    }
    fn session_variable(&self, name: &str, key: &str) -> Result<Option<String>> {
        // The session's whole environment: a listing that succeeds and lacks
        // the key proves it unset, which a failed lookup of the key alone
        // could not tell from an unreadable terminal.
        let listing =
            platform::capture("tmux", &["show-environment", "-t", &format!("={name}")])
                .ok_or_else(|| format!("{name}: the terminal's environment cannot be read"))?;
        let prefix = format!("{key}=");
        Ok(listing
            .lines()
            .find_map(|line| line.strip_prefix(&prefix))
            .filter(|value| !value.is_empty())
            .map(str::to_owned))
    }
    fn notify(&self, client: &str, text: &str) {
        platform::silent(
            "tmux",
            &["display-message", "-c", client, "-d", "4000", text],
        );
    }
    fn forward_key(&self, pane: &str, key: &str) {
        let _ = Command::new("tmux")
            .args(["send-keys", "-t", pane, key])
            .status();
    }
    fn describe(&self, operation: &DryRun) -> String {
        match operation {
            DryRun::ListJson => "tmux list-clients -F \"#{client_session}\\t#{client_tty}\"\ntmux list-sessions -F \"#{session_name}\\t#{session_attached}\\t#{session_created}\\t#{session_id}\\t#{HGS_LAUNCH_ID}\"".into(),
            DryRun::List => "tmux list-sessions".into(),
            // Every word as the shell reads it: a session name is free text (zerus-sa9qn.38).
            DryRun::Attach { session, client } => format!(
                "tmux set-environment -t {target} HGS_CLIENT {client}\ntmux -u attach-session -t {target}",
                target = platform::quote(&format!("={session}")),
                client = platform::quote(client)
            ),
            DryRun::AttachExisting { session, run_id } => format!(
                "tmux attach existing session {}{}",
                platform::quote(session),
                run_id
                    .map(|id| format!(" (run {})", platform::quote(id)))
                    .unwrap_or_default()
            ),
            DryRun::Kill { session } => format!(
                "tmux kill-session -t {}",
                platform::quote(&format!("={session}"))
            ),
            DryRun::Launch(spec) => format!(
                "tmux -u new-session {}{} -s {} -c {} -- {}",
                if spec.detached { "-d " } else { "" },
                spec.env
                    .iter()
                    .map(|v| format!("-e {}", platform::quote(&super::masked(v))))
                    .collect::<Vec<_>>()
                    .join(" "),
                platform::quote(&spec.name),
                platform::quote(&spec.cwd),
                platform::join(&spec.command)
            ),
            // `quote`'s words, which fish also reads exactly (zerus-sa9qn.43).
            DryRun::Start(spec) => {
                let mut display = vec!["tmux".to_owned()];
                display.extend(start_args(spec));
                platform::join(&display)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::PaneField::*;
    use super::*;

    /// zerus-5sl7r: only the server that created the pane, by its pid and
    /// start time, says anything of it, and a live pane is the launch only
    /// while it runs the launched process (pid and kernel birth). Another
    /// server at the same socket path, a missing or refused socket and every
    /// other failure are unknown; a pane respawned with another process is
    /// replaced. The answers are tmux 3.8's own, read from private servers
    /// (the review's respawn included).
    #[test]
    fn only_the_creating_server_says_its_pane_is_gone() {
        use TerminalState::{Absent, Alive, DeadPane, Replaced, Unknown};
        let mut pane = launched(b"517721:1795037025:%1:517731\n").unwrap();
        assert_eq!(
            pane,
            Launched {
                server: "517721:1795037025".into(),
                pane: "%1".into(),
                process: 517731,
                birth: None
            }
        );
        pane.birth = Some("7000".into());
        let births = |pid: u32| match pid {
            517731 => Some("7000".to_owned()),
            517918 => Some("7100".to_owned()),
            _ => None,
        };
        for (status, stdout, state) in [
            (Some(0), "517721:1795037025:%1:0:517731\n", Alive),
            // Its command ended and the pane is kept: tmux clears its pid.
            (Some(0), "517721:1795037025:%1:1:\n", DeadPane),
            // `respawn-pane` after it ended: another process in the same pane.
            (Some(0), "517721:1795037025:%1:0:517918\n", Replaced),
            // The creating server, with or without other sessions, has no
            // such pane any more.
            (Some(0), "517721:1795037025:::\n", Absent),
            // A sessionless server started at the same path.
            (Some(0), "517929:1795037025:::\n", Unknown),
            (Some(0), "517929:1795037025:%1:0:517731\n", Unknown),
            // The same pid with another start: another server.
            (Some(0), "517721:1795037999:::\n", Unknown),
            (Some(0), "::::\n", Unknown),
            (Some(0), "", Unknown),
            (Some(0), "517721:1795037025:%1:0\n", Unknown),
            (Some(0), "517721:1795037025:%1:0:517731:extra\n", Unknown),
            (Some(0), "517721:1795037025:%1::517731\n", Unknown),
            // As a client without a UTF-8 locale reads tabs.
            (Some(0), "517721_1795037025_%1_0_517731\n", Unknown),
            // A missing or refused socket, a replacement's `no current
            // target`, a failed program or a signal.
            (Some(1), "", Unknown),
            (Some(127), "517721:1795037025:::\n", Unknown),
            (None, "517721:1795037025:::\n", Unknown),
        ] {
            assert_eq!(
                observed_state(&pane, status, stdout.as_bytes(), births),
                state,
                "{status:?} {stdout:?}"
            );
        }
        // The launched pid with another birth (reused), or one that cannot
        // be read now or at the launch, is not the launched process alive.
        let live = b"517721:1795037025:%1:0:517731\n";
        assert_eq!(
            observed_state(&pane, Some(0), live, |_| Some("7999".into())),
            Replaced
        );
        assert_eq!(observed_state(&pane, Some(0), live, |_| None), Unknown);
        let unborn = Launched {
            birth: None,
            ..pane.clone()
        };
        assert_eq!(observed_state(&unborn, Some(0), live, births), Unknown);
        for unnamed in [
            "",
            "\n",
            "517721:1795037025\n",
            "517721:1795037025:%1\n",
            "517721:1795037025:%1:\n",
            "517721:1795037025:%1:0\n",
            ":1795037025:%1:517731\n",
            "517721::%1:517731\n",
            "517721:1795037025:1:517731\n",
            "517721:1795037025:%1:517731:x\n",
            "517721:1795037025:%1:x\n",
            // As a client without a UTF-8 locale, or an older format, reads.
            "517721_1795037025_%1_517731\n",
            "517721\t1795037025\t%1\t517731\n",
        ] {
            assert_eq!(launched(unnamed.as_bytes()), None, "{unnamed:?}");
        }
        // A pane's process is the server's child; nothing else is its birth.
        let own = std::process::id();
        assert_eq!(child_birth(own, "4294967294:1795037025"), None);
        assert_eq!(child_birth(own, "x"), None);
        let parent = platform::process_identity(own).unwrap().parent;
        assert_eq!(
            child_birth(own, &format!("{parent}:1795037025")),
            Some(platform::process_identity(own).unwrap().birth)
        );
        let spec = LaunchSpec {
            name: "claude/p/t".into(),
            cwd: "/tmp".into(),
            env: vec!["A=1".into()],
            command: vec!["/x/hgs".into()],
            detached: true,
        };
        let observed = start_observed_args(&spec);
        assert_eq!(&observed[..5], ["new-session", "-d", "-P", "-F", LAUNCHED]);
        assert_eq!(observed[5..], start_args(&spec)[2..]);
    }

    /// zerus-sa9qn.38: `--dry-run` prints commands a person may paste; each
    /// line passes a hostile session name, folder and variable to tmux as the
    /// same single words and runs nothing else.
    #[test]
    fn dry_runs_print_hostile_names_as_single_words() {
        let names = [
            "claude/p/x;touch pwned;#",
            "claude/$(touch pwned)/t",
            "claude/p/`touch pwned`",
            "claude/p/it's \"q\" | touch pwned",
            "claude/p/x\ntouch pwned",
            // zerus-sa9qn.43: fish reads \' inside single quotes.
            "claude/p/x\\'; touch pwned; #",
            "claude/p/a\\b",
        ];
        for name in names {
            let spec = LaunchSpec {
                name: name.into(),
                cwd: format!("/work/{name}"),
                env: vec![format!("HGS_SESSION={name}")],
                command: vec!["/x/hgs".into(), "0".into()],
                detached: true,
            };
            let launch = Tmux.describe(&DryRun::Launch(&spec));
            let start = Tmux.describe(&DryRun::Start(&spec));
            for (line, args) in [
                (start, vec![start_args(&spec)]),
                (
                    Tmux.describe(&DryRun::Attach {
                        session: name,
                        client: "local",
                    }),
                    vec![
                        vec![
                            "set-environment".to_owned(),
                            "-t".into(),
                            format!("={name}"),
                            "HGS_CLIENT".into(),
                            "local".into(),
                        ],
                        vec![
                            "-u".into(),
                            "attach-session".into(),
                            "-t".into(),
                            format!("={name}"),
                        ],
                    ],
                ),
                (
                    Tmux.describe(&DryRun::Kill { session: name }),
                    vec![vec!["kill-session".into(), "-t".into(), format!("={name}")]],
                ),
                (
                    launch,
                    vec![["-u", "new-session", "-d", "-e"]
                        .map(String::from)
                        .into_iter()
                        .chain([
                            format!("HGS_SESSION={name}"),
                            "-s".into(),
                            name.into(),
                            "-c".into(),
                            format!("/work/{name}"),
                            "--".into(),
                            "/x/hgs".into(),
                            "0".into(),
                        ])
                        .collect()],
                ),
            ] {
                let folder = tempfile::tempdir().unwrap();
                let script = format!("tmux() {{ printf '%s\\0' \"$@\"; printf '\\n'; }}\n{line}");
                let out = Command::new("/bin/sh")
                    .arg("-c")
                    .arg(script)
                    .current_dir(folder.path())
                    .env("PATH", "/usr/bin:/bin")
                    .output()
                    .unwrap();
                assert!(
                    !folder.path().join("pwned").exists(),
                    "{line:?} ran a payload"
                );
                let calls: Vec<Vec<String>> = String::from_utf8(out.stdout)
                    .unwrap()
                    .split_terminator("\0\n")
                    .map(|call| call.split('\0').map(str::to_owned).collect())
                    .collect();
                assert_eq!(calls, args, "{line}");
                // fish (modelled) reads each line as the same tmux call.
                let tmux: Vec<_> = args
                    .iter()
                    .map(|call| [vec!["tmux".to_owned()], call.clone()].concat())
                    .collect();
                assert_eq!(crate::platform::fish_commands(&line), Ok(tmux), "{line}");
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_test_image_never_hands_its_runner_recipe_to_tmux() {
        crate::self_exec::private_probe("state::backend::tmux::tests::runner_submission_probe");
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "run by a_test_image_never_hands_its_runner_recipe_to_tmux"]
    fn runner_submission_probe() {
        crate::self_exec::probe_child("state::backend::tmux::tests::runner_submission_probe");
        // The real recipe still renders; only its submission is refused.
        let runner = crate::state::storage::runner_executable().unwrap();
        let spec = |detached| LaunchSpec {
            name: "probe".into(),
            cwd: "/".into(),
            env: vec![],
            command: [
                "/bin/sh",
                "-lic",
                "exec \"$0\" --run \"$@\"",
                &runner,
                "0",
                "/bin/true",
            ]
            .map(str::to_owned)
            .to_vec(),
            detached,
        };
        // No fake is installed: this is the real tmux backend fork,
        // relaunch and launch use.
        let backend = super::super::current();
        assert_eq!(
            backend.start(&spec(true)).unwrap_err(),
            crate::self_exec::TEST_IMAGE
        );
        for detached in [true, false] {
            let error = backend.launch(&spec(detached)).err().unwrap();
            assert_eq!(error.message, crate::self_exec::TEST_IMAGE);
        }
    }

    #[test]
    fn pane_queries_keep_their_formats_and_targets() {
        assert_eq!(
            format(&[Tty, CursorX, CursorY, InMode, CurrentCommand, Dead, RunMarker, SessionName, BracketedPaste]),
            "#{pane_tty}\t#{cursor_x}\t#{cursor_y}\t#{pane_in_mode}\t#{pane_current_command}\t#{pane_dead}\t#{@hgs_run}\t#{session_name}\t#{bracket_paste_flag}"
        );
        assert_eq!(
            format(&[
                SessionId, PaneId, ServerPid, PanePid, Width, Height, CursorX, CursorY, InMode, Dead,
                RunMarker, SessionName, BracketedPaste, Tty,
            ]),
            "#{session_id}\t#{pane_id}\t#{pid}\t#{pane_pid}\t#{pane_width}\t#{pane_height}\t#{cursor_x}\t#{cursor_y}\t#{pane_in_mode}\t#{pane_dead}\t#{@hgs_run}\t#{session_name}\t#{bracket_paste_flag}\t#{pane_tty}"
        );
        assert_eq!(
            pane_info_args(Target::Name("claude/p/t"), &[SessionId, RunMarker]),
            [
                "display-message",
                "-p",
                "-t",
                "=claude/p/t:",
                "#{session_id}\t#{@hgs_run}"
            ]
        );
        assert_eq!(
            pane_info_args(Target::Session("$4"), &[SessionName]),
            ["display-message", "-p", "-t", "$4", "#{session_name}"]
        );
        assert_eq!(session_target(Target::Name("a/b")), "=a/b");
        assert_eq!(session_target(Target::Session("$4")), "$4");
    }

    #[test]
    fn keys_and_pastes_keep_their_argument_order() {
        assert_eq!(send_args("%1", &[]), [["send-keys", "-t", "%1"]]);
        assert_eq!(
            send_args("%1", &[Key::Named("C-x"), Key::Named("C-s")]),
            [["send-keys", "-t", "%1", "C-x", "C-s"]]
        );
        assert_eq!(
            send_args("%1", &[Key::Literal("/clear")]),
            [["send-keys", "-t", "%1", "-l", "/clear"]]
        );
        assert_eq!(
            send_args("%1", &[Key::Literal("/model"), Key::Named("Enter")]),
            [
                owned(&["send-keys", "-t", "%1", "-l", "/model"]),
                owned(&["send-keys", "-t", "%1", "Enter"])
            ]
        );
        assert_eq!(
            paste_args("b", "%1", Paste::Composer),
            ["paste-buffer", "-p", "-r", "-d", "-b", "b", "-t", "%1"]
        );
        assert_eq!(
            paste_args("b", "%1", Paste::Terminal { bracketed: true }),
            ["paste-buffer", "-r", "-d", "-b", "b", "-t", "%1", "-p"]
        );
        assert_eq!(
            paste_args("b", "%1", Paste::Terminal { bracketed: false }),
            ["paste-buffer", "-r", "-d", "-b", "b", "-t", "%1"]
        );
    }

    #[test]
    fn a_supervisor_reports_its_launch_complete() {
        assert!(!Tmux.completes_launch_reports());
    }

    #[test]
    fn launches_render_as_before() {
        let mut spec = LaunchSpec {
            name: "claude/p/t".into(),
            cwd: "/tmp/a b".into(),
            env: vec!["A=1".into(), "B=two words".into()],
            command: owned(&[
                "/bin/bash",
                "-lic",
                "exec \"$0\" --run \"$@\"",
                "/x/hgs",
                "0",
                "claude",
            ]),
            detached: false,
        };
        assert_eq!(
            start_args(&spec),
            [
                "new-session",
                "-d",
                "-s",
                "claude/p/t",
                "-c",
                "/tmp/a b",
                "-e",
                "A=1",
                "-e",
                "B=two words",
                "--",
                "/usr/bin/env",
                "--",
                "/bin/bash",
                "-lic",
                "exec \"$0\" --run \"$@\"",
                "/x/hgs",
                "0",
                "claude",
            ]
        );
        assert_eq!(
            Tmux.describe(&DryRun::Start(&spec)),
            "tmux new-session -d -s claude/p/t -c '/tmp/a b' -e A=1 -e 'B=two words' -- /usr/bin/env -- /bin/bash -lic 'exec \"$0\" --run \"$@\"' /x/hgs 0 claude"
        );
        assert_eq!(
            launch_args(&spec),
            [
                "-u",
                "new-session",
                "-e",
                "A=1",
                "-e",
                "B=two words",
                "-s",
                "claude/p/t",
                "-c",
                "/tmp/a b",
                "--",
                "/usr/bin/env",
                "--",
                "/bin/bash",
                "-lic",
                "exec \"$0\" --run \"$@\"",
                "/x/hgs",
                "0",
                "claude",
            ]
        );
        assert_eq!(
            Tmux.describe(&DryRun::Launch(&spec)),
            "tmux -u new-session -e A=1 -e 'B=two words' -s claude/p/t -c '/tmp/a b' -- /bin/bash -lic 'exec \"$0\" --run \"$@\"' /x/hgs 0 claude"
        );
        spec.detached = true;
        spec.env.clear();
        assert_eq!(launch_args(&spec)[..3], ["-u", "new-session", "-d"]);
        assert!(Tmux
            .describe(&DryRun::Launch(&spec))
            .starts_with("tmux -u new-session -d  -s claude/p/t -c '/tmp/a b' -- "));
    }

    #[test]
    fn a_dry_run_masks_a_secret_in_the_launch_environment() {
        let spec = LaunchSpec {
            name: "claude/p/t".into(),
            cwd: "/work".into(),
            env: vec![
                "ANTHROPIC_AUTH_TOKEN=sk-secret".into(),
                "ANTHROPIC_BASE_URL=http://192.0.2.10:8080".into(),
            ],
            command: vec!["claude".into()],
            detached: false,
        };
        let line = Tmux.describe(&DryRun::Launch(&spec));
        assert!(line.contains("ANTHROPIC_AUTH_TOKEN=***"), "{line}");
        assert!(!line.contains("sk-secret"), "the secret never reaches a dry-run: {line}");
        assert!(line.contains("ANTHROPIC_BASE_URL=http://192.0.2.10:8080"), "{line}");
    }

    #[test]
    fn direct_argv_has_no_default_shell_and_keeps_literal_final_semicolons() {
        let arguments = direct_command(
            owned(&["new-session", "-c", "/private/dir;", "-e", "A=x;"]),
            &owned(&[
                "/private/program",
                ";",
                "\\;",
                "words with space",
                "quote'",
                "$(not-a-command)",
                "",
            ]),
        );
        assert_eq!(
            arguments,
            owned(&[
                "new-session",
                "-c",
                "/private/dir\\;",
                "-e",
                "A=x\\;",
                "--",
                "/usr/bin/env",
                "--",
                "/private/program",
                "\\;",
                "\\\\;",
                "words with space",
                "quote'",
                "$(not-a-command)",
                ""
            ])
        );
    }
}
