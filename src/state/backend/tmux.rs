//! The tmux backend. Every call spawns plain `tmux` by name, so a PATH shim and an
//! inherited `TMUX` or `TMUX_TMPDIR` select the server; no call passes `-L`, `-S`
//! or `-f`. Exact names are `=NAME` for session commands and `=NAME:` (the
//! session's pane) for pane and option commands. On Linux a `new-session` that
//! may start the server runs in a scope of its own when hgs runs in a user unit
//! (`scope`); it still spawns that same `tmux` command.
use super::{
    ClientExit, ClientRow, DryRun, InputClaim, InputState, Key, LaunchSpec, Listing, PaneField,
    PaneInfo, PanePath, PaneRow, Paste, SessionRow, StageErrors, Target, TerminalBackend,
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
    checked(Command::new("tmux").args(args).output(), check)
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
        let result = run(&mut scope.command(args));
        if scope.started() || !rerun(&result) {
            return result;
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = rerun;
    run(Command::new("tmux").args(args))
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

fn pane_info_args(target: Target, fields: &[PaneField]) -> Vec<String> {
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
    arguments.push(crate::state::join_command(&spec.command));
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
        platform::join(&spec.command),
    ]);
    arguments
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
        let output = tmux(
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
    fn session_name(&self, id: &str) -> Result<Option<String>> {
        let output = tmux(
            &owned(&["display-message", "-p", "-t", id, "#{session_name}"]),
            false,
        )?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
    }
    fn pane_info(&self, target: Target, fields: &[PaneField]) -> Result<PaneInfo> {
        let output = tmux(&pane_info_args(target, fields), true)?;
        Ok(PaneInfo::new(
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
    }
    fn probe(&self, target: Target, fields: &[PaneField]) -> Option<PaneInfo> {
        let arguments = pane_info_args(target, fields);
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
        platform::capture("tmux", &arguments).map(PaneInfo::new)
    }
    fn start(&self, spec: &LaunchSpec) -> Result<()> {
        let output = starting(&start_args(spec), Command::output, |_| true);
        checked(output, true).map(drop)
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
        let info = self.pane_info(
            Target::Pane(claim.pane),
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
        let tty_output = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "tty=,pgid=,tpgid=,comm="])
            .output()
            .map_err(|e| e.to_string())?;
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
    fn on_terminal(&self, pid: &str, tty: &str) -> Result<bool> {
        let output = Command::new("ps")
            .args(["-p", pid, "-o", "tty="])
            .output()
            .map_err(|e| e.to_string())?;
        Ok(output.status.success()
            && !tty.is_empty()
            && String::from_utf8_lossy(&output.stdout).trim() == tty.trim_start_matches("/dev/"))
    }
    fn capture(&self, pane: &str, mode: super::Capture) -> Result<Vec<u8>> {
        let arguments = match mode {
            super::Capture::Plain => owned(&["capture-pane", "-p", "-t", pane]),
            super::Capture::Styled => owned(&["capture-pane", "-p", "-e", "-t", pane]),
        };
        Ok(tmux(&arguments, true)?.stdout)
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
        for arguments in send_args(pane, keys) {
            tmux(&arguments, true)?;
        }
        Ok(())
    }
    fn stage(&self, buffer: &str, text: &[u8], errors: StageErrors) -> Result<bool> {
        let mut loader = Command::new("tmux")
            .args(["load-buffer", "-b", buffer, "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(match errors {
                StageErrors::Read => Stdio::piped(),
                StageErrors::Discard => Stdio::null(),
            })
            .spawn()
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
            DryRun::Attach { session, client } => format!(
                "tmux set-environment -t ={session} HGS_CLIENT {client}\ntmux -u attach-session -t ={session}"
            ),
            DryRun::AttachExisting { session, run_id } => format!(
                "tmux attach existing session {}{}",
                platform::quote(session),
                run_id
                    .map(|id| format!(" (run {})", platform::quote(id)))
                    .unwrap_or_default()
            ),
            DryRun::Kill { session } => format!("tmux kill-session -t ={session}"),
            DryRun::Launch(spec) => format!(
                "tmux -u new-session {}{} -s {} -c {} -- {}",
                if spec.detached { "-d " } else { "" },
                spec.env
                    .iter()
                    .map(|v| format!("-e {}", super::masked(v)))
                    .collect::<Vec<_>>()
                    .join(" "),
                spec.name,
                spec.cwd,
                platform::join(&spec.command)
            ),
            DryRun::Start(spec) => {
                let mut display = vec!["tmux".into()];
                display.extend(start_args(spec));
                crate::state::join_command(&display)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::PaneField::*;
    use super::*;

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
                "/bin/bash -lic 'exec \"$0\" --run \"$@\"' /x/hgs 0 claude",
            ]
        );
        assert_eq!(
            Tmux.describe(&DryRun::Start(&spec)),
            "tmux new-session -d -s claude/p/t -c '/tmp/a b' -e 'A=1' -e 'B=two words' '/bin/bash -lic '\\''exec \"$0\" --run \"$@\"'\\'' /x/hgs 0 claude'"
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
                "/bin/bash -lic 'exec \"$0\" --run \"$@\"' /x/hgs 0 claude",
            ]
        );
        assert_eq!(
            Tmux.describe(&DryRun::Launch(&spec)),
            "tmux -u new-session -e A=1 -e B=two words -s claude/p/t -c /tmp/a b -- /bin/bash -lic 'exec \"$0\" --run \"$@\"' /x/hgs 0 claude"
        );
        spec.detached = true;
        spec.env.clear();
        assert_eq!(launch_args(&spec)[..3], ["-u", "new-session", "-d"]);
        assert!(Tmux
            .describe(&DryRun::Launch(&spec))
            .starts_with("tmux -u new-session -d  -s claude/p/t -c /tmp/a b -- "));
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
}
