//! On-demand refresh using the selected account's installed Codex app-server.
use super::*;
use crate::state::{codex_cache_info, CodexCacheInfo};
use chrono::{DateTime, TimeDelta, Utc};
use std::{
    io::{BufRead, BufReader, Read},
    process::{Child, ExitStatus, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

const MAX_OUTPUT: u64 = 1024 * 1024;
// Match the native ModelsManager file-cache TTL so model/list will fetch.
const CACHE_AGE: TimeDelta = TimeDelta::minutes(5);
const SERVER_LIMIT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
enum Failure {
    Version,
    DryRun,
    Busy,
    SignIn,
    Unavailable,
    Missing,
}

impl Failure {
    fn detail(self) -> &'static str {
        match self {
            Self::Version => "Could not determine the installed Codex version.",
            Self::DryRun => "Saving the choice can refresh a stale catalog.",
            Self::Busy => {
                "A catalog refresh is already in progress. Wait for it to finish and retry."
            }
            Self::SignIn => "Codex could not authenticate. Sign in again and retry.",
            Self::Unavailable => "Codex could not refresh this account's catalog.",
            Self::Missing => "The model is unavailable in this account's native catalog.",
        }
    }
}

struct Version {
    label: String,
    numbers: [u64; 3],
}

fn version_numbers(label: &str) -> Option<[u64; 3]> {
    let mut parts = label.split(['-', '+']).next()?.split('.');
    let result = [
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ];
    parts.next().is_none().then_some(result)
}

fn codex(home: &Path, native: bool) -> Command {
    let mut command = inspection::native_command("codex", home, native);
    command.env("CODEX_HOME", home);
    command
}

fn installed_version(home: &Path, native: bool) -> Option<Version> {
    let mut command = codex(home, native);
    command.arg("--version");
    let output = inspection::capture(command, b"", 2).ok()?;
    let label = std::str::from_utf8(&output)
        .ok()?
        .trim()
        .strip_prefix("codex-cli ")?;
    let numbers = version_numbers(label)?;
    let label = label
        .chars()
        .take(64)
        .filter(|ch| ch.is_ascii_alphanumeric() || ".-+".contains(*ch))
        .collect();
    Some(Version { label, numbers })
}

fn check_sign_in(home: &Path, native: bool) -> std::result::Result<(), Failure> {
    let mut command = codex(home, native);
    command
        .current_dir(home)
        .args(["login", "status"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut child = platform::spawn_group(&mut command).map_err(|_| Failure::Unavailable)?;
    match settle(&mut child, deadline).and_then(|status| status.code()) {
        Some(0) => Ok(()),
        Some(1) => Err(Failure::SignIn),
        _ => Err(Failure::Unavailable),
    }
}

fn stale(info: &CodexCacheInfo, installed: &Version, now: DateTime<Utc>) -> bool {
    info.client_version
        .as_deref()
        .and_then(version_numbers)
        .is_none_or(|version| version < installed.numbers)
        || info
            .fetched_at
            .is_none_or(|date| now.signed_duration_since(date) > CACHE_AGE)
}

pub(super) fn refresh_missing(
    config: &Config,
    id: &str,
    profile: &Profile,
    home: &Path,
    native: bool,
    model: &str,
    dry: bool,
) -> Result<Value> {
    let mut info = codex_cache_info(home);
    let version = installed_version(home, native);
    let result = (|| {
        let installed = version.as_ref().ok_or(Failure::Version)?;
        if !stale(&info, installed, Utc::now()) {
            return Err(Failure::Missing);
        }
        if dry {
            return Err(Failure::DryRun);
        }
        let account_home = fs::canonicalize(home).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Failure::SignIn
            } else {
                Failure::Unavailable
            }
        })?;
        check_sign_in(&account_home, native)?;
        // One writer across processes and across different Zerus catalogs that
        // point to the same Codex home. Never unlink the lock while others use it.
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(account_home.join(".zerus-model-catalog.lock"))
            .map_err(|_| Failure::Unavailable)?;
        lock.try_lock_exclusive().map_err(|error| {
            if platform::lock_contended(&error) {
                Failure::Busy
            } else {
                Failure::Unavailable
            }
        })?;
        info = codex_cache_info(home);
        let models = model_defaults::models("codex", home);
        if model_defaults::has_model(&models, model) {
            return Ok(models);
        }
        if !stale(&info, installed, Utc::now()) {
            return Err(Failure::Missing);
        }

        let attempt = model_list(&account_home, native, &info, installed);
        let refreshed = codex_cache_info(home);
        let unchanged = refreshed == info;
        info = refreshed;
        attempt?;
        // The RPC can return bundled fallback rows even when the fetch failed.
        // Require a current native cache write and validate the exact same home.
        if unchanged || stale(&info, installed, Utc::now()) {
            return Err(Failure::Unavailable);
        }
        let models = model_defaults::models("codex", home);
        if !model_defaults::has_model(&models, model) {
            return Err(Failure::Missing);
        }
        Ok(models)
    })();
    result.map_err(|failure| {
        let date = info.fetched_at.map(|date| date.to_rfc3339());
        let version = version.as_ref().map(|version| version.label.as_str());
        Error::new(
            1,
            format!(
                "{} On {}, account {} ({}), catalog fetched {}, installed Codex {}.",
                failure.detail(),
                config.host(),
                profile.label,
                id,
                date.as_deref().unwrap_or("at an unknown time"),
                version.unwrap_or("version unknown"),
            ),
        )
    })
}

fn model_list(
    home: &Path,
    native: bool,
    previous: &CodexCacheInfo,
    installed: &Version,
) -> std::result::Result<(), Failure> {
    let mut command = codex(home, native);
    command
        .current_dir(home)
        .args([
            "-c",
            "analytics.enabled=false",
            "--disable",
            "remote_plugin",
            "app-server",
            "--listen",
            "stdio://",
        ])
        .env("NO_COLOR", "1")
        .env("RUST_LOG", "error")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let deadline = Instant::now() + SERVER_LIMIT;
    let mut child = platform::spawn_group(&mut command).map_err(|_| Failure::Unavailable)?;

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let mut stdin = child.stdin.take().expect("piped stdin");
    let (replies, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout.take(MAX_OUTPUT))
            .lines()
            .map_while(std::result::Result::ok)
        {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(value @ 1..=2) = message["id"].as_u64() {
                // Keep only protocol acknowledgements and classified errors.
                let result = &message["result"];
                let reply = if !result.is_object() {
                    Err(
                        if auth_error(message["error"]["message"].as_str().unwrap_or("")) {
                            Failure::SignIn
                        } else {
                            Failure::Unavailable
                        },
                    )
                } else {
                    Ok(value == 1 || result["data"].is_array())
                };
                if replies.send((value, reply)).is_err() {
                    break;
                }
            }
        }
    });
    let (auth, auth_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let reported = BufReader::new(stderr.take(MAX_OUTPUT))
            .lines()
            .map_while(std::result::Result::ok)
            .any(|line| auth_error(&line));
        let _ = auth.send(reported);
    });
    let exchange = (|| {
        let mut send = |value: Value| writeln!(stdin, "{value}").map_err(|_| Failure::Unavailable);
        let reply = |id| loop {
            let (actual, result) = receive(&rx, deadline)?;
            if actual == id {
                return result;
            }
        };
        send(json!({"id":1,"method":"initialize","params":{"clientInfo":{
            "name":"hgs_zerus","title":"HGS Zerus","version":env!("CARGO_PKG_VERSION")
        }}}))?;
        reply(1)?;
        send(json!({"method":"initialized"}))?;
        if auth_rx.try_recv() == Ok(true) {
            return Err(Failure::SignIn);
        }
        send(json!({"id":2,"method":"model/list","params":{}}))?;
        if !reply(2)? {
            return Err(Failure::Unavailable);
        }
        Ok(())
    })();
    // Startup discovery may already be fetching the catalog when model/list
    // replies. Give that original process time to write before closing stdin.
    let mut rejected = false;
    if exchange.is_ok() {
        while Instant::now() < deadline {
            if auth_rx.try_recv() == Ok(true) {
                rejected = true;
                break;
            }
            let current = codex_cache_info(home);
            if current != *previous && !stale(&current, installed, Utc::now()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    drop(stdin);
    let exit = settle(&mut child, deadline);
    drop(child);
    if rejected || matches!(receive(&auth_rx, deadline), Ok(true)) {
        return Err(Failure::SignIn);
    }
    exchange?;
    if !exit.is_some_and(|status| status.success()) {
        return Err(Failure::Unavailable);
    }
    Ok(())
}

fn auth_error(message: &str) -> bool {
    // model/list can report a credential failure in diagnostics and still
    // return bundled rows. Never forward native errors or user data verbatim.
    let message = message.to_ascii_lowercase();
    message.contains("sign in again")
        || message.contains("not logged in")
        || message.contains("authentication required")
        || message.contains("auth error: 401")
        || (message.contains("failed to refresh available models")
            && (message.contains("401 unauthorized") || message.contains("403 forbidden")))
        || (message.contains("failed to refresh token") && message.contains("400 bad request"))
}

fn receive<T>(rx: &mpsc::Receiver<T>, deadline: Instant) -> std::result::Result<T, Failure> {
    rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| Failure::Unavailable)
}

fn settle(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    while let Ok(status) = child.try_wait() {
        if status.is_some() {
            return status;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    platform::kill_process_group(child);
    let _ = child.wait();
    None
}
