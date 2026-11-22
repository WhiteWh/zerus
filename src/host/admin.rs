//! `hgs session-host install | status | stop | uninstall [--json]` (spec D2):
//! enabling local sessions on a Windows computer and managing its session host.
//!
//! The command exists only in Windows builds, runs before local sessions are
//! enabled and is never forwarded to another machine; on Unix the word still
//! starts a program of that name. `install` stages the stable CLI and a host image
//! and writes the marker (`staging`). `status` reports the marker, the staged
//! files and the running host, which it asks over the host pipe and never
//! starts. `stop` stops an idle host; the host refuses while sessions run.
//! `uninstall` refuses while sessions run, then removes the marker and leaves
//! the staged files.
//!
//! Argument handling and the reports are portable and tested everywhere; the
//! host connection and the command itself are Windows-only. `local_host` is the
//! same finding as `hgs ls --json` reports it in the local box (D2).
use super::{
    protocol::{HelloReply, HostStatus},
    staging::{self, CliReport, Image, ImageReport, InstallReport, Layout, Pair},
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const USAGE: &str = "usage: hgs session-host install | status | start | stop | uninstall | keep | autostart on|off|status [--json]";
/// The line `hgs --help` adds on Windows.
pub const HELP: &str = "       hgs session-host install | status | start | stop | uninstall | keep | autostart on|off|status [--json]   local sessions on this Windows computer (start: a host that stays for SSH peers; keep, autostart: without the desktop app)\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Install,
    Status,
    // WhiteWh fork: start a host that stays without sessions.
    Start,
    Stop,
    Uninstall,
    // WhiteWh fork: no desktop app on this computer (src/host/keeper.rs).
    Keep,
    /// The detached keeper itself (internal).
    KeepRun,
    AutostartOn,
    AutostartOff,
    AutostartStatus,
}

/// A parsed `hgs session-host` command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub action: Action,
    pub json: bool,
}

/// The request in `args` (after `session-host`), None when they are not one.
pub fn parse(args: &[String]) -> Option<Request> {
    let (action, mut options) = args.split_first()?;
    if action == "keep" && options.len() == 1 && options[0] == "--run" {
        return Some(Request { action: Action::KeepRun, json: false });
    }
    let action = match action.as_str() {
        "keep" => Action::Keep,
        "autostart" => {
            let (choice, rest) = options.split_first()?;
            options = rest;
            match choice.as_str() {
                "on" => Action::AutostartOn,
                "off" => Action::AutostartOff,
                "status" => Action::AutostartStatus,
                _ => return None,
            }
        }
        "install" => Action::Install,
        "start" => Action::Start,
        "status" => Action::Status,
        "stop" => Action::Stop,
        "uninstall" => Action::Uninstall,
        _ => return None,
    };
    let json = match options {
        [] => false,
        [option] if option == "--json" => true,
        _ => return None,
    };
    Some(Request { action, json })
}

/// The session host as `status` finds it; `state` names it as `local_host` does
/// in listings (D2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Host {
    /// Local sessions are not enabled and no host runs.
    NotInstalled,
    /// No host runs: nothing serves its pipe.
    Stopped,
    Running {
        hello: Box<HelloReply>,
        status: HostStatus,
    },
    /// A host may run but did not answer: busy, refused, another protocol.
    Unavailable(String),
}

impl Host {
    /// The host as found where local sessions are `enabled` or not: a stopped
    /// host of a computer that is not enabled is not installed.
    pub fn found(self, enabled: bool) -> Self {
        match self {
            Self::Stopped if !enabled => Self::NotInstalled,
            host => host,
        }
    }

    /// `local_host` in the local box of `hgs ls --json` (D2): the state and the
    /// running host's process, protocol, ConPTY, integrity level and Windows
    /// session, null while no host answers; an unavailable host adds its error.
    pub fn listing_json(&self) -> Value {
        match self {
            Self::Running { hello, status } => json!({
                "state": self.state(),
                "pid": hello.pid,
                "protocol": hello.protocol,
                "conpty": status.conpty,
                "integrity": hello.integrity.name,
                "windows_session": hello.windows_session,
            }),
            _ => {
                let mut value = json!({
                    "state": self.state(),
                    "pid": null,
                    "protocol": null,
                    "conpty": null,
                    "integrity": null,
                    "windows_session": null,
                });
                if let Self::Unavailable(error) = self {
                    value["error"] = json!(error);
                }
                value
            }
        }
    }

    pub fn state(&self) -> &'static str {
        match self {
            Self::NotInstalled => "not-installed",
            Self::Stopped => "stopped",
            Self::Running { .. } => "running",
            Self::Unavailable(_) => "unavailable",
        }
    }

    fn json(&self) -> Value {
        match self {
            Self::Running { hello, status } => json!({
                "state": self.state(),
                "pid": hello.pid,
                "version": hello.version,
                "protocol": hello.protocol,
                "conpty": status.conpty,
                "integrity": hello.integrity.name,
                "windows_session": hello.windows_session,
                "in_job": hello.in_job,
                "desktop_job": hello.desktop_job,
                "image": hello.image,
                "sessions": status.sessions,
                "clients": status.clients,
            }),
            Self::Unavailable(error) => json!({"state": self.state(), "error": error}),
            _ => json!({"state": self.state()}),
        }
    }

    fn text(&self) -> String {
        match self {
            Self::NotInstalled => "not installed".into(),
            Self::Stopped => "stopped".into(),
            Self::Running { hello, status } => format!(
                "running (PID {}, hgs {}, protocol {}, {}, {}, {} ConPTY, {} integrity, Windows session {}{})",
                hello.pid,
                hello.version,
                hello.protocol,
                count(status.sessions, "session"),
                count(status.clients, "client"),
                status.conpty,
                hello.integrity.name,
                hello.windows_session,
                match (hello.in_job, hello.desktop_job) {
                    (false, _) => "",
                    (true, true) => ", in the desktop's job",
                    (true, false) => ", in a job",
                }
            ),
            Self::Unavailable(error) => format!("unavailable: {error}"),
        }
    }
}

fn count(number: u32, noun: &str) -> String {
    format!("{number} {noun}{}", if number == 1 { "" } else { "s" })
}

fn conpty(bundled: bool) -> &'static str {
    if bundled {
        "bundled"
    } else {
        "inbox"
    }
}

fn shown(path: &Path) -> String {
    path.display().to_string()
}

fn image_json(image: &Image) -> Value {
    json!({
        "name": image.name,
        "path": shown(&image.path),
        "sha256": image.sha256,
        "version": image.version,
        "conpty": conpty(image.bundled),
    })
}

/// `install --json`: what was staged, for this build of version `version`.
pub fn install_json(report: &InstallReport, layout: &Layout, version: &str) -> Value {
    let CliReport {
        path,
        sha256,
        changed,
        removed,
        kept,
        lore,
    } = &report.cli;
    let ImageReport {
        image,
        pair,
        changed: image_changed,
        removed: images_removed,
        kept: images_kept,
    } = &report.image;
    let mut staged = image_json(image);
    staged["pair"] = json!(match pair {
        Pair::Bundled => "bundled",
        Pair::Absent => "absent",
        Pair::Incomplete => "incomplete",
    });
    staged["changed"] = json!(image_changed);
    staged["removed"] = json!(images_removed);
    staged["kept"] = json!(images_kept);
    json!({
        "enabled": true,
        "version": version,
        "marker": shown(&layout.marker),
        "marker_created": report.enabled,
        "cli": {
            "path": shown(path),
            "sha256": sha256,
            "version": version,
            "changed": changed,
            "removed_aside": removed,
            "kept_aside": kept.iter().map(|path| shown(path)).collect::<Vec<_>>(),
        },
        // Task #420: what staging lore did; a newer or richer one stays.
        "lore": lore_json(lore),
        "image": staged,
        "frontend": report.frontend.as_ref().map(|binding| json!({
            "path": shown(&binding.frontend.path),
            "cli_current": true,
            "frontend_current": true,
        })),
    })
}

/// Task #420: the lore outcome of an install.
fn lore_json(lore: &staging::LoreStage) -> Value {
    match lore {
        staging::LoreStage::Absent => json!({"action":"absent"}),
        staging::LoreStage::Same => json!({"action":"unchanged"}),
        staging::LoreStage::Installed { backup } => json!({"action":"installed","backup":backup.as_deref().map(shown)}),
        staging::LoreStage::Kept { reason } => json!({"action":"kept","reason":reason}),
    }
}

/// `install` for a person.
pub fn install_text(report: &InstallReport, layout: &Layout, version: &str) -> String {
    let updated = |changed: bool| if changed { "updated" } else { "unchanged" };
    let image = &report.image;
    let mut text = format!(
        "Local sessions are enabled on this computer (hgs {version}).\n\
         \x20 CLI         {} ({})\n\
         \x20 Host image  {} ({} ConPTY, {})\n\
         \x20 Marker      {}\n",
        report.cli.path.display(),
        updated(report.cli.changed),
        image.image.path.display(),
        conpty(image.image.bundled),
        updated(image.changed),
        layout.marker.display(),
    );
    if image.pair == Pair::Incomplete {
        text.push_str(
            "Only one file of the bundled ConPTY pair (conpty.dll, OpenConsole.exe) sits next to \
             this hgs.exe; the session host uses inbox ConPTY.\n",
        );
    }
    // Task #420: a newer or richer lore stays; a replaced one keeps a .bak.
    match &report.cli.lore {
        staging::LoreStage::Kept { reason } => text.push_str(&format!(
            "  lore        kept the installed lore: {reason}\n"
        )),
        staging::LoreStage::Installed { backup: Some(backup) } => text.push_str(&format!(
            "  lore        updated; the previous one is kept as {}\n",
            backup.display()
        )),
        _ => {}
    }
    if !report.cli.kept.is_empty() {
        text.push_str(&format!(
            "{} of the stable CLI still run{}; a later install removes {}.\n",
            match report.cli.kept.len() {
                1 => "An earlier copy".to_owned(),
                count => format!("{count} earlier copies"),
            },
            if report.cli.kept.len() == 1 { "s" } else { "" },
            if report.cli.kept.len() == 1 {
                "it"
            } else {
                "them"
            },
        ));
    }
    for name in &image.kept {
        text.push_str(&format!(
            "Host image {name} stays while a session host runs from it.\n"
        ));
    }
    text
}

/// What `status` reports about the stable CLI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cli {
    pub path: PathBuf,
    /// None when it is not installed.
    pub sha256: Option<String>,
    /// Known when a staged image holds the same build.
    pub version: Option<String>,
}

/// Everything `status` reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub enabled: bool,
    pub marker: PathBuf,
    pub cli: Cli,
    /// The image `current` names.
    pub image: Option<Image>,
    pub host: Host,
    /// The app bound to the staged CLI, whether the binding still names that
    /// CLI, and whether the file at the app's path is still the bound app.
    /// Informational only; admission proves the pair itself.
    pub frontend: Option<(PathBuf, bool, bool)>,
}

impl Status {
    /// The staged files in `layout`, with the host as found.
    pub fn gather(layout: &Layout, host: Host) -> Self {
        let sha256 = staging::sha256(&layout.cli).ok();
        let version = sha256
            .as_deref()
            .and_then(|hash| staging::version_of(&layout.images, hash));
        let image = staging::current_image_in(&layout.images).and_then(|folder| {
            let name = folder.file_name()?.to_str()?.to_owned();
            Image::read(&layout.images, &name)
        });
        let enabled = layout.enabled();
        Self {
            enabled,
            marker: layout.marker.clone(),
            cli: Cli {
                path: layout.cli.clone(),
                sha256,
                version,
            },
            image,
            host: host.found(enabled),
            frontend: crate::desktop::portable_frontend::status(layout),
        }
    }

    pub fn json(&self, version: &str) -> Value {
        json!({
            "enabled": self.enabled,
            "version": version,
            "marker": shown(&self.marker),
            "cli": {
                "path": shown(&self.cli.path),
                "installed": self.cli.sha256.is_some(),
                "sha256": self.cli.sha256,
                "version": self.cli.version,
            },
            "image": self.image.as_ref().map(image_json),
            "host": self.host.json(),
            "frontend": self.frontend.as_ref().map(|(path, cli, frontend)| json!({
                "path": shown(path),
                "cli_current": cli,
                "frontend_current": frontend,
            })),
        })
    }

    pub fn text(&self) -> String {
        let cli = match (&self.cli.sha256, &self.cli.version) {
            (None, _) => format!("not installed ({})", self.cli.path.display()),
            (Some(_), Some(version)) => format!("{} (hgs {version})", self.cli.path.display()),
            (Some(hash), None) => format!(
                "{} (sha256 {})",
                self.cli.path.display(),
                staging::image_name(hash)
            ),
        };
        let image = match &self.image {
            None => "none".into(),
            Some(image) => format!(
                "{} (hgs {}, {} ConPTY)",
                image.path.display(),
                image.version.as_deref().unwrap_or("of unknown version"),
                conpty(image.bundled)
            ),
        };
        format!(
            "Local sessions  {} ({})\n\
             Stable CLI      {cli}\n\
             Host image      {image}\n\
             Session host    {}\n",
            if self.enabled {
                "enabled"
            } else {
                "not enabled"
            },
            self.marker.display(),
            self.host.text(),
        )
    }
}

#[cfg(windows)]
pub(crate) use self::native::{dispatch, host_state, local_host, start_resident};

#[cfg(windows)]
mod native {
    use super::*;
    use crate::{
        cli::{Error, Result},
        config::Config,
        host::{
            client::{self, ClientError, Connection},
            protocol::{Done, ErrorCode, Op, Role},
        },
        platform,
    };
    use std::{
        io,
        time::{Duration, Instant},
    };

    const VERSION: &str = env!("CARGO_PKG_VERSION");
    /// How long an answer may take.
    const PATIENCE: Duration = Duration::from_secs(5);
    /// How long `stop` waits for a stopped host to exit.
    const EXIT_WAIT: Duration = Duration::from_secs(10);

    /// The connection to this user's host for `state`, None when no host
    /// runs. Never starts one, and never creates the state directory: without
    /// it, or without the secret a host creates before it listens, none runs.
    fn open(state: &Path) -> std::result::Result<Option<Connection>, String> {
        let state = match platform::canonical(state) {
            Ok(state) => state,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("{}: {error}", state.display())),
        };
        match client::connect(&state, Role::Cli) {
            Ok(connection) => {
                connection
                    .set_read_timeout(Some(PATIENCE))
                    .map_err(|error| error.to_string())?;
                Ok(Some(connection))
            }
            Err(ClientError::NoHost) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    /// The host for `state`, asked for its status; never started.
    fn query(state: &Path) -> Host {
        match open(state) {
            Ok(None) => Host::Stopped,
            Ok(Some(mut connection)) => match connection.call::<HostStatus>(Op::HostStatus) {
                Ok(status) => Host::Running {
                    hello: Box::new(connection.host().clone()),
                    status,
                },
                Err(error) => Host::Unavailable(error.message),
            },
            Err(error) => Host::Unavailable(error),
        }
    }

    fn print_json(value: &Value) {
        println!("{value}");
    }

    /// `local_host` for the local box of `hgs ls --json` (D2): the host of
    /// `state` as found, never started.
    pub(crate) fn local_host(state: &Path) -> Value {
        query(state)
            .found(platform::local_sessions())
            .listing_json()
    }

    pub(crate) fn dispatch(config: &Config, args: &[String], dry: bool) -> Result<i32> {
        let request = parse(args).ok_or_else(|| Error::new(1, USAGE))?;
        if dry {
            return Err(Error::new(1, "hgs session-host does not support --dry-run"));
        }
        // The paths `platform::local_sessions` reads, from the loaded configuration.
        let layout = Layout::new(&config.home, &config.dir);
        match request.action {
            Action::Install => install(&layout, request.json),
            Action::Status => {
                let status = Status::gather(&layout, query(&config.state));
                if request.json {
                    print_json(&status.json(VERSION));
                } else {
                    print!("{}", status.text());
                }
                Ok(0)
            }
            Action::Start => start(&config.state, request.json),
            Action::Keep => crate::host::keeper::keep(config, &layout, request.json),
            Action::KeepRun => crate::host::keeper::run(config, &layout),
            Action::AutostartOn => crate::host::keeper::autostart(config, &layout, Some(true), request.json),
            Action::AutostartOff => crate::host::keeper::autostart(config, &layout, Some(false), request.json),
            Action::AutostartStatus => crate::host::keeper::autostart(config, &layout, None, request.json),
            Action::Stop => stop(&config.state, request.json),
            Action::Uninstall => uninstall(&layout, &config.state, request.json),
        }
    }

    fn install(layout: &Layout, json: bool) -> Result<i32> {
        let source = std::env::current_exe()?;
        // Only the app beside this hgs, starting it outside any agent tree,
        // records itself as this CLI's portable frontend.
        let report = staging::install_bound(
            layout,
            &source,
            VERSION,
            crate::desktop::portable_frontend::record,
        )
        .map_err(|error| Error::new(1, format!("cannot enable local sessions: {error}")))?;
        // Task #227: an install or update restarted the sessions; one sweep
        // gives the attempts a restart left unconfirmed back to their members.
        if let Err(error) = crate::state::teams::dispatcher::recover_after_update() {
            eprintln!("hgs: the team recovery sweep after the install failed: {error}");
        }
        if json {
            let mut value = install_json(&report, layout, VERSION);
            // Task #43: why the app beside this hgs was not bound.
            if report.frontend.is_none() {
                if let Some(reason) = crate::desktop::portable_frontend::last_refusal() {
                    // Task #53: its fixed code, which the app logs and names.
                    value["frontend_refusal_code"] = serde_json::json!(crate::desktop::portable_frontend::refusal_code(&reason));
                    value["frontend_refusal"] = serde_json::json!(reason);
                }
            }
            print_json(&value);
        } else {
            print!("{}", install_text(&report, layout, VERSION));
        }
        Ok(0)
    }

    /// WhiteWh fork: `hgs session-host start` brings up a host that stays
    /// without sessions until `stop`, so other computers (a Windows SSH peer)
    /// can start sessions here: an SSH session itself never starts the host.
    /// A host that already runs is left as it is.
    fn start(state: &Path, json: bool) -> Result<i32> {
        // The host takes this process's environment (D8).
        // SAFETY: set before any thread of this short-lived command starts.
        unsafe { std::env::set_var(crate::host::registry::RESIDENT_ENV, "1") };
        let running = !start_resident(state)?;
        if json {
            print_json(&json!({"was_running": running, "running": true}));
        } else if running {
            println!("The session host already runs.");
        } else {
            println!("The session host runs and stays until hgs session-host stop.");
        }
        Ok(0)
    }

    /// A host that stays, started unless one runs; true when this call
    /// started it. The caller has set `RESIDENT_ENV`, which the host takes
    /// from this process's environment (D8).
    pub(crate) fn start_resident(state: &Path) -> Result<bool> {
        if !platform::local_sessions() {
            return Err(Error::new(1, "Local sessions are not enabled on this computer: run hgs session-host install first."));
        }
        if matches!(query(state), Host::Running { .. }) {
            return Ok(false);
        }
        std::fs::create_dir_all(state)?;
        let state = platform::canonical(state)?;
        client::connect_or_start(&state, Role::Cli, &|| {
            crate::host::gate::host_image(&state).map_err(client::ClientError::Start)
        })
        .map_err(|error| Error::new(1, format!("cannot start the session host: {error}")))?;
        Ok(true)
    }

    /// The host's state word as `status` finds it, never starting one.
    pub(crate) fn host_state(state: &Path) -> &'static str {
        query(state).found(platform::local_sessions()).state()
    }

    fn stop(state: &Path, json: bool) -> Result<i32> {
        let unavailable =
            |error: String| Error::new(1, format!("cannot reach the session host: {error}"));
        let Some(mut connection) = open(state).map_err(unavailable)? else {
            if json {
                print_json(&json!({"was_running": false, "pid": null, "exited": true}));
            } else {
                println!("No session host runs.");
            }
            return Ok(0);
        };
        let pid = connection.host().pid;
        // Pinned first, so a reused process ID is never mistaken for the host.
        // The host has just answered, so a process that cannot be pinned is not
        // known to be gone: its exit is then never reported.
        let pinned = platform::PinnedProcess::pin(pid, &connection.host().created).ok();
        match connection.call::<Done>(Op::StopHost) {
            Ok(Done {}) => {}
            // The host says why it keeps running.
            Err(error) if error.code == ErrorCode::Busy => {
                return Err(Error::new(1, error.message))
            }
            Err(error) => return Err(unavailable(error.message)),
        }
        drop(connection);
        let deadline = Instant::now() + EXIT_WAIT;
        let exited = loop {
            match &pinned {
                Some(process) if process.alive() && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Some(process) => break !process.alive(),
                // Its identity did not match the hello or it could not be
                // opened: whether it exited is unknown.
                None => break false,
            }
        };
        if json {
            print_json(&json!({"was_running": true, "pid": pid, "exited": exited}));
        } else if exited {
            println!("Stopped the session host (PID {pid}).");
        } else {
            println!("The session host (PID {pid}) is stopping.");
        }
        Ok(0)
    }

    fn uninstall(layout: &Layout, state: &Path, json: bool) -> Result<i32> {
        let host = query(state);
        match &host {
            Host::Running { status, .. } if status.sessions > 0 => {
                return Err(Error::new(
                    1,
                    format!(
                        "{} run{} in the session host on this computer; end {} before \
                         disabling local sessions",
                        count(status.sessions, "session"),
                        if status.sessions == 1 { "s" } else { "" },
                        if status.sessions == 1 { "it" } else { "them" },
                    ),
                ))
            }
            Host::Unavailable(error) => {
                return Err(Error::new(
                    1,
                    format!(
                        "cannot check the session host for running sessions: {error}; \
                         local sessions stay enabled"
                    ),
                ))
            }
            _ => {}
        }
        let removed = staging::disable(&layout.marker)
            .map_err(|error| Error::new(1, format!("{}: {error}", layout.marker.display())))?;
        if json {
            print_json(&json!({
                "enabled": false,
                "marker": shown(&layout.marker),
                "marker_removed": removed,
                "host": host.state(),
            }));
        } else if removed {
            println!(
                "Local sessions are disabled on this computer; the staged CLI and host images stay in place."
            );
        } else {
            println!("Local sessions were not enabled on this computer.");
        }
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::protocol::Integrity;
    use std::fs;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn only_the_known_actions_and_json_parse() {
        for (words, action) in [
            (&["install"][..], Action::Install),
            (&["status"], Action::Status),
            (&["start"], Action::Start),
            (&["stop"], Action::Stop),
            (&["uninstall"], Action::Uninstall),
            (&["keep"], Action::Keep),
            (&["autostart", "on"], Action::AutostartOn),
            (&["autostart", "off"], Action::AutostartOff),
            (&["autostart", "status"], Action::AutostartStatus),
        ] {
            assert_eq!(
                parse(&args(words)),
                Some(Request {
                    action,
                    json: false
                })
            );
            let mut with_json = args(words);
            with_json.push("--json".into());
            assert_eq!(parse(&with_json), Some(Request { action, json: true }));
        }
        // The detached keeper's own line, which no one types.
        assert_eq!(parse(&args(&["keep", "--run"])), Some(Request { action: Action::KeepRun, json: false }));
        // `hgs --help` on Windows names exactly what parses.
        assert!(HELP.contains(USAGE.trim_start_matches("usage: ")));
        for words in [
            &[][..],
            &["restart"],
            &["Install"],
            &["--json", "install"],
            &["install", "--json", "--json"],
            &["status", "extra"],
            &["stop", "-d"],
            &["autostart"],
            &["autostart", "maybe"],
            &["keep", "--run", "--json"],
        ] {
            assert_eq!(parse(&args(words)), None, "{words:?}");
        }
    }

    fn hello() -> HelloReply {
        HelloReply {
            pid: 4242,
            created: "133000000000000000".into(),
            image: r"C:\Users\a\.local\share\hgs\host\0123456789abcdef\hgs.exe".into(),
            image_hash: "00".repeat(32),
            version: "1.2.3".into(),
            protocol: 1,
            capabilities: Vec::new(),
            integrity: Integrity::from_rid(Integrity::MEDIUM),
            windows_session: 1,
            in_job: false,
            desktop_job: false,
        }
    }

    #[test]
    fn status_reports_files_marker_and_host() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(
            &dir.path().join("home").to_string_lossy(),
            &dir.path().join("config"),
        );
        let empty = Status::gather(&layout, Host::Stopped);
        // Without the marker a stopped host reads as not installed.
        assert_eq!(empty.host, Host::NotInstalled);
        let value = empty.json("1.2.3");
        assert_eq!(value["enabled"], false);
        assert_eq!(value["cli"]["installed"], false);
        assert_eq!(value["cli"]["sha256"], Value::Null);
        assert_eq!(value["image"], Value::Null);
        assert_eq!(value["host"], json!({"state": "not-installed"}));
        assert_eq!(value["frontend"], Value::Null);
        assert!(empty.text().contains("not enabled"));

        let source = dir.path().join("build").join(staging::EXECUTABLE);
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, b"build").unwrap();
        let report = staging::install(&layout, &source, "1.2.3").unwrap();
        let value = install_json(&report, &layout, "1.2.3");
        assert_eq!(value["enabled"], true);
        assert_eq!(value["marker_created"], true);
        assert_eq!(value["cli"]["changed"], true);
        assert_eq!(value["cli"]["sha256"], report.cli.sha256);
        assert_eq!(value["image"]["pair"], "absent");
        assert_eq!(value["image"]["conpty"], "inbox");
        assert_eq!(value["image"]["name"], report.image.image.name);
        // A plain install records no app binding.
        assert_eq!(value["frontend"], Value::Null);
        assert!(install_text(&report, &layout, "1.2.3").starts_with("Local sessions are enabled"));

        let stopped = Status::gather(&layout, Host::Stopped);
        let value = stopped.json("1.2.3");
        assert_eq!(value["enabled"], true);
        assert_eq!(value["cli"]["version"], "1.2.3");
        assert_eq!(value["image"]["version"], "1.2.3");
        assert_eq!(value["image"]["sha256"], report.cli.sha256);
        assert_eq!(value["host"], json!({"state": "stopped"}));

        let running = Status::gather(
            &layout,
            Host::Running {
                hello: Box::new(hello()),
                status: HostStatus {
                    sessions: 2,
                    clients: 1,
                    conpty: "bundled".into(),
                },
            },
        );
        assert_eq!(
            running.json("1.2.3")["host"],
            json!({"state": "running", "pid": 4242, "version": "1.2.3", "protocol": 1,
                   "conpty": "bundled", "integrity": "medium", "windows_session": 1,
                   "in_job": false, "desktop_job": false, "image": hello().image, "sessions": 2,
                   "clients": 1})
        );
        assert!(running
            .text()
            .contains("running (PID 4242, hgs 1.2.3, protocol 1, 2 sessions, 1 client,"));
        let unavailable = Status::gather(&layout, Host::Unavailable("busy".into()));
        assert_eq!(
            unavailable.json("1.2.3")["host"],
            json!({"state": "unavailable", "error": "busy"})
        );
    }

    #[test]
    fn listings_carry_the_local_host_with_every_field() {
        let none = |state: &str| {
            json!({"state": state, "pid": null, "protocol": null, "conpty": null,
                   "integrity": null, "windows_session": null})
        };
        // Enablement, not the host, decides between not installed and stopped.
        assert_eq!(
            Host::Stopped.found(false).listing_json(),
            none("not-installed")
        );
        assert_eq!(Host::Stopped.found(true).listing_json(), none("stopped"));
        assert_eq!(Host::NotInstalled.listing_json(), none("not-installed"));
        let mut unavailable = none("unavailable");
        unavailable["error"] = json!("the session host on this computer runs elevated");
        assert_eq!(
            Host::Unavailable("the session host on this computer runs elevated".into())
                .found(true)
                .listing_json(),
            unavailable
        );
        // A running host of a computer whose marker is gone still runs.
        let running = Host::Running {
            hello: Box::new(HelloReply {
                integrity: Integrity::from_rid(Integrity::HIGH),
                windows_session: 2,
                ..hello()
            }),
            status: HostStatus {
                sessions: 1,
                clients: 1,
                conpty: "inbox".into(),
            },
        };
        assert_eq!(
            running.found(false).listing_json(),
            json!({"state": "running", "pid": 4242, "protocol": 1, "conpty": "inbox",
                   "integrity": "high", "windows_session": 2})
        );
    }
}
