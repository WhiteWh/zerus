//! WhiteWh fork: a Windows computer that other computers manage, with no
//! desktop app running on it.
//!
//! The desktop keeps the session host up for SSH peers (`resident_host.rs`,
//! Settings > Sessions); an SSH session never starts one. Without the desktop,
//! `hgs session-host keep` does the same from the signed-in user's own session:
//! it starts itself again detached and windowless (`keep --run`), holds one
//! lock so a second keeper leaves at once, and every minute starts the host
//! unless it runs and the team worker while a team is active and no worker
//! runs. It holds no window, webview or GPU context and sleeps between checks.
//! The session host supervises the recovery worker itself (spec D14).
//!
//! `hgs session-host autostart on` writes the keeper into this user's Run key,
//! so it starts at sign-in; `off` removes it and asks a running keeper to
//! leave. Over SSH, `autostart on` only registers it: the keeper then starts at
//! the next sign-in, as an SSH session's token must not be the host's.
use super::staging::Layout;
use crate::{
    cli::{Error, Result},
    config::Config,
    platform,
};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime},
};

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Zerus session host";
/// How often the host and the team worker are checked.
const INTERVAL: Duration = Duration::from_secs(60);
/// How often a sleeping keeper looks for a stop request.
const STOP_POLL: Duration = Duration::from_secs(5);
/// Hide the console window (CREATE_NO_WINDOW).
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn lock_path(config: &Config) -> PathBuf {
    config.state.join("session-host-keeper.lock")
}

fn stop_path(config: &Config) -> PathBuf {
    config.state.join("session-host-keeper.stop")
}

fn over_ssh() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_CLIENT").is_some()
}

/// Whether a keeper holds its lock.
fn running(config: &Config) -> bool {
    crate::status::locked(&lock_path(config))
}

/// The command line the Run key holds: the stable CLI, never this image.
fn run_command(layout: &Layout) -> String {
    format!("\"{}\" session-host keep", layout.cli.display())
}

/// `hgs session-host keep`: starts the detached keeper unless one runs.
pub(crate) fn keep(config: &Config, layout: &Layout, json: bool) -> Result<i32> {
    if over_ssh() {
        return Err(Error::new(
            1,
            "hgs session-host keep runs in this computer's own sign-in, not over SSH: \
             use hgs session-host autostart on, which starts it at the next sign-in",
        ));
    }
    if !platform::local_sessions() {
        return Err(Error::new(1, "Local sessions are not enabled on this computer: run hgs session-host install first."));
    }
    let was_running = running(config);
    if !was_running {
        let _ = fs::remove_file(stop_path(config));
        spawn(layout)?;
    }
    if json {
        println!("{}", json!({"was_running": was_running, "running": true}));
    } else if was_running {
        println!("The session host keeper already runs.");
    } else {
        println!("The session host keeper runs: it keeps the session host and the team worker up without the desktop app.");
    }
    Ok(0)
}

/// Starts `keep --run` from the stable CLI, detached and windowless, from the
/// home folder so it holds no other folder.
fn spawn(layout: &Layout) -> Result<()> {
    let program = if layout.cli.is_file() { layout.cli.clone() } else { std::env::current_exe()? };
    let mut command = Command::new(program);
    command
        .args(["session-host", "keep", "--run"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(home) = platform::home() {
        command.current_dir(home);
    }
    // Zerus' own process: no agent's markers or Git configuration.
    for name in ["TMUX", "HGS_HOST_PANE", "HGS_SESSION", "HGS_RUN_ID", "DSH_SESSION_ID", "GIT_CONFIG_PARAMETERS", "CLAUDECODE"] {
        command.env_remove(name);
    }
    platform::spawn_detached(&mut command)?;
    Ok(())
}

/// `hgs session-host keep --run`: the keeper itself.
pub(crate) fn run(config: &Config, layout: &Layout) -> Result<i32> {
    if over_ssh() {
        return Err(Error::new(1, "the session host keeper never runs from an SSH session"));
    }
    fs::create_dir_all(&config.state)?;
    let file = fs::OpenOptions::new().create(true).truncate(false).write(true).open(lock_path(config))?;
    if fs2::FileExt::try_lock_exclusive(&file).is_err() {
        return Ok(0);
    }
    let _lock = platform::FileLock::held(file);
    let _ = fs::remove_file(stop_path(config));
    // The host takes this process's environment (D8).
    // SAFETY: set before this process starts any thread.
    unsafe { std::env::set_var(crate::host::registry::RESIDENT_ENV, "1") };
    let started = modified(&layout.cli);
    loop {
        if let Err(error) = super::admin::start_resident(&config.state) {
            eprintln!("hgs session-host keep: {}", error.message);
        }
        if let Err(error) = keep_team_worker() {
            eprintln!("hgs session-host keep: team worker: {error}");
        }
        let until = Instant::now() + INTERVAL;
        while Instant::now() < until {
            if stop_path(config).exists() {
                let _ = fs::remove_file(stop_path(config));
                return Ok(0);
            }
            std::thread::sleep(STOP_POLL);
        }
        // An update replaced the stable CLI: its keeper carries on.
        if modified(&layout.cli) != started {
            drop(_lock);
            spawn(layout)?;
            return Ok(0);
        }
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

/// The team worker while a team is active and none runs; it leaves by itself
/// once no team runs.
fn keep_team_worker() -> anyhow::Result<()> {
    use crate::state::teams::{dispatcher, store::Store};
    if crate::status::locked(&dispatcher::lock_path()) {
        return Ok(());
    }
    if Store::open(false)?.active_teams(1)?.is_empty() {
        return Ok(());
    }
    dispatcher::ensure_worker()
}

/// `hgs session-host autostart on|off|status`.
pub(crate) fn autostart(config: &Config, layout: &Layout, on: Option<bool>, json: bool) -> Result<i32> {
    match on {
        Some(true) => {
            if !platform::local_sessions() {
                return Err(Error::new(1, "Local sessions are not enabled on this computer: run hgs session-host install first."));
            }
            reg(&["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &run_command(layout), "/f"])?;
            if !over_ssh() && !running(config) {
                let _ = fs::remove_file(stop_path(config));
                spawn(layout)?;
            }
        }
        Some(false) => {
            if registered().is_some() {
                reg(&["delete", RUN_KEY, "/v", RUN_VALUE, "/f"])?;
            }
            if running(config) {
                fs::write(stop_path(config), b"")?;
            }
        }
        None => {}
    }
    let command = registered();
    let keeper = running(config) && !(on == Some(false));
    let host = super::admin::host_state(&config.state);
    if json {
        println!(
            "{}",
            json!({"autostart": command.is_some(), "command": command, "keeper": keeper, "host": host})
        );
    } else {
        match &command {
            Some(command) => println!("Starts at sign-in: {command}"),
            None => println!("Does not start at sign-in."),
        }
        println!("Keeper: {}", if keeper { "running" } else if on == Some(false) && running(config) { "stopping" } else { "not running" });
        println!("Session host: {host}");
        if on == Some(true) && over_ssh() {
            println!("Over SSH the keeper starts at the next sign-in of this user, not now.");
        }
    }
    Ok(0)
}

/// The Run value, None when absent.
fn registered() -> Option<String> {
    let output = reg_command(&["query", RUN_KEY, "/v", RUN_VALUE]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().find_map(|line| {
        let line = line.trim_start();
        let rest = line.strip_prefix(RUN_VALUE)?.trim_start();
        let rest = rest.strip_prefix("REG_SZ")?;
        Some(rest.trim().to_owned())
    })
}

fn reg(args: &[&str]) -> Result<()> {
    let output = reg_command(args).output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(Error::new(
        1,
        format!("reg {} failed: {}", args[0], String::from_utf8_lossy(&output.stderr).trim()),
    ))
}

fn reg_command(args: &[&str]) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new("reg.exe");
    command.args(args).stdin(Stdio::null()).creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_run_key_starts_the_stable_cli_keeper() {
        let layout = Layout::new(r"C:\Users\someone", Path::new(r"C:\Users\someone\.config\hgs"));
        assert_eq!(run_command(&layout), r#""C:\Users\someone\.local\bin\hgs.exe" session-host keep"#);
    }
}
