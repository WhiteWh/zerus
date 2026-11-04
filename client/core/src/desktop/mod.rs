//! Read-only identity challenge on Next's existing instance socket. Neither a
//! matching hello nor cached configuration is a writer permit or drain proof.
use crate::{
    actions::{Exit, Invocation},
    run::run_streamed,
};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use zerus_desktop_migration::canonical_uuid;

pub mod activation;
pub mod approval_audit;
pub mod cache_maintenance;
pub mod creation_clone;
pub mod drain;
pub mod notification_control;
pub mod notifications;
pub mod ownership;
pub mod pending_reads;
pub mod producer;
pub mod profile;
pub mod relay;
pub mod service;
pub mod startup;
pub mod ui_requests;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{probe_at, probe_producer_at, probe_ui_request_at, same_user, temporary_dir_from};
#[cfg(windows)]
pub mod windows;

pub const ABI: &str = "desktop-delegation-v1";
pub const HELP_MARKER: &str = "Desktop owner ABI: desktop-delegation-v1";
pub const REPLY_BYTES: usize = 4096;
pub const PROBE_WAIT: Duration = Duration::from_secs(1);
const SERVICE_HELP: &str = "Desktop service ABI: desktop-v1";

pub fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A fixed 59-byte line, including LF, inside the original 64-byte input cap.
pub fn query(nonce: &str) -> Option<Vec<u8>> {
    canonical_uuid(nonce).then(|| format!("{ABI} {nonce}\n").into_bytes())
}
pub fn query_nonce(line_without_lf: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(line_without_lf).ok()?;
    let nonce = text.strip_prefix(ABI)?.strip_prefix(' ')?;
    canonical_uuid(nonce).then_some(nonce)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Build {
    pub version: String,
    pub source_commit: String,
    pub source_dirty: Option<bool>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub abi: String,
    pub version: u32,
    pub nonce: String,
    pub application: String,
    pub gui: String,
    pub build: Build,
    pub instance_epoch: String,
    /// Currently None: this endpoint cannot assert a writer epoch yet.
    pub writer_epoch: Option<String>,
    pub configuration: Option<String>,
    pub live: bool,
    pub replay: bool,
}
impl Hello {
    pub fn identity_matches(&self, nonce: &str, configuration: &str) -> bool {
        self.abi == ABI
            && self.version == 1
            && canonical_uuid(nonce)
            && self.nonce == nonce
            && canonical_uuid(&self.instance_epoch)
            && self.writer_epoch.as_deref().is_none_or(canonical_uuid)
            && digest(configuration)
            && self.configuration.as_deref() == Some(configuration)
            && self.application == "com.hgdev.hgs-tray"
            && self.gui == "tauri"
            && self.live
            && !self.replay
            && !self.build.version.is_empty()
            && self.build.version.len() <= 128
            && !self.build.version.chars().any(char::is_control)
            && matches!(self.build.source_commit.len(), 40 | 64)
            && self
                .build
                .source_commit
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && self.build.source_dirty == Some(false)
    }
}

/// Construct once at startup. The listener only reads this cache; it never
/// spawns a CLI or performs configuration I/O in response to a socket query.
pub struct Responder {
    application: String,
    build: Build,
    instance_epoch: String,
    live: bool,
    replay: bool,
    configuration: OnceLock<String>,
    ownership_mode: OnceLock<ownership::Mode>,
    producer: Mutex<producer::Cached>,
    ui_requests: ui_requests::Cache,
    notification_focus: notification_control::native::Cache,
}
impl Responder {
    /// Cached identity data only; no I/O, worker permission or writer grant.
    pub fn cached_configuration(&self) -> Option<&str> {
        self.configuration.get().map(String::as_str)
    }
    pub fn new(
        application: String,
        build: Build,
        instance_epoch: String,
        live: bool,
        replay: bool,
    ) -> Self {
        Self {
            application,
            build,
            instance_epoch,
            live,
            replay,
            configuration: OnceLock::new(),
            ownership_mode: OnceLock::new(),
            producer: Mutex::default(),
            ui_requests: ui_requests::Cache::default(),
            notification_focus: notification_control::native::Cache::default(),
        }
    }
    pub fn configure_once(&self, configuration: String) -> bool {
        digest(&configuration) && self.configuration.set(configuration).is_ok()
    }
    /// Native startup only, before canonical claim and any writer creation.
    /// This cached value is not read from a request's JSON or an environment
    /// variable by the responder, and cannot change within an instance.
    pub fn configure_ownership_mode_once(&self, mode: ownership::Mode) -> bool {
        self.live && !self.replay && self.ownership_mode.set(mode).is_ok()
    }
    pub fn reply(&self, line_without_lf: &[u8]) -> Option<Vec<u8>> {
        if let Some(attempt) = ui_requests::query_attempt(line_without_lf) {
            return self.ui_reply(attempt);
        }
        if let Some(nonce) = producer::query_nonce(line_without_lf) {
            return self.producer_reply(nonce, Instant::now());
        }
        let nonce = query_nonce(line_without_lf)?;
        if !canonical_uuid(&self.instance_epoch) {
            return None;
        }
        let mut bytes = serde_json::to_vec(&self.hello(nonce)).ok()?;
        bytes.push(b'\n');
        (bytes.len() <= REPLY_BYTES).then_some(bytes)
    }
    fn hello(&self, nonce: &str) -> Hello {
        Hello {
            abi: ABI.into(),
            version: 1,
            nonce: nonce.into(),
            application: self.application.clone(),
            gui: "tauri".into(),
            build: self.build.clone(),
            instance_epoch: self.instance_epoch.clone(),
            writer_epoch: None,
            configuration: self.configuration.get().cloned(),
            live: self.live,
            replay: self.replay,
        }
    }
}

/// Call ONCE, off the GUI thread, with the selected local CLI. Replay must not
/// call this. Help comes first: unknown commands can launch sessions on old hgs.
/// A failed probe stays unknown for this instance; it has no retry loop.
pub fn selected_configuration(hgs: &Path, environment: &[(&str, &str)]) -> Option<String> {
    let read = |args: &[&str], cap| {
        let invocation = Invocation {
            args: args.iter().map(|s| (*s).into()).collect(),
            stdin: None,
            timeout: Duration::from_secs(5),
            request_id: None,
        };
        match run_streamed(hgs, &invocation, environment, cap, |_| true) {
            Ok(Exit::Code {
                code: 0, stdout, ..
            }) => Some(stdout),
            _ => None,
        }
    };
    let help = read(&["--help"], 256 << 10)?;
    if !std::str::from_utf8(&help)
        .ok()?
        .lines()
        .any(|s| s.trim() == SERVICE_HELP)
    {
        return None;
    }
    #[derive(Deserialize)]
    struct Status {
        abi: String,
        configuration: String,
    }
    let status: Status =
        serde_json::from_slice(&read(&["desktop", "status", "--json"], 64 << 10)?).ok()?;
    (status.abi == "desktop-v1" && digest(&status.configuration)).then_some(status.configuration)
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Observation {
    Absent,
    /// An endpoint exists but no trustworthy live peer could be established.
    Unknown,
    /// A connected peer ignored/rejected the query or supplied unknown identity.
    LiveLegacyOrUnknown,
    LiveNext {
        hello: Box<Hello>,
    },
}

#[cfg(test)]
mod tests;
