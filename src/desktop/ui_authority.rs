//! Frontend requests are issued by an original native UI child, never
//! by the generic CLI request lane or by a JSON role/confirmation flag.
#[cfg(unix)]
use super::owner;
use super::store::Location;
use crate::{config::Config, platform};
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use std::{
    cell::Cell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use zerus_client_core::desktop::ui_requests::{Envelope, Reply, Request};

#[cfg(target_os = "linux")]
mod linux_manager;
#[cfg(any(target_os = "macos", test))]
mod macos_ancestry;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) mod registration;
#[cfg(windows)]
#[path = "ui_authority/registration_windows.rs"]
pub(super) mod registration;

#[cfg(any(all(not(windows), not(target_os = "macos")), test))]
use std::collections::BTreeSet;
#[cfg(windows)]
use zerus_client_core::desktop::windows::{
    process::{Ancestry, Original},
    Peer,
};

#[cfg(any(not(windows), test))]
const ANCESTORS: usize = 32;
const WAIT: Duration = Duration::from_secs(2);
const ACTIVATION_PROOF_INTERVAL: Duration = Duration::from_millis(500);

pub(super) struct Scope {
    location: Location,
    frontend_images: Vec<PathBuf>,
}
impl std::ops::Deref for Scope {
    type Target = Location;
    fn deref(&self) -> &Location {
        &self.location
    }
}
impl Scope {
    pub fn new(config: &Config, location: &Location) -> Self {
        let home = Path::new(&config.home);
        #[cfg(not(windows))]
        let mut frontend_images = vec![home.join(".local/bin/hgs-tray")];
        #[cfg(windows)]
        let mut frontend_images = vec![std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"))
            .join("Programs/Zerus/hgs-tray.exe")];
        if let Ok(cli) = std::env::current_exe() {
            if let Some(folder) = cli.parent() {
                frontend_images.push(folder.join(if cfg!(windows) {
                    "hgs-tray.exe"
                } else {
                    "hgs-tray"
                }));
            }
            #[cfg(target_os = "macos")]
            if let Some(bundle) = cli
                .ancestors()
                .find(|p| p.extension().is_some_and(|e| e == "app"))
            {
                frontend_images.push(bundle.join("Contents/MacOS/hgs-tray"));
            }
        }
        #[cfg(target_os = "macos")]
        for bundle in [
            home.join("Applications/hgs-tray.app"),
            home.join("Applications/Zerus.app"),
            PathBuf::from("/Applications/Zerus.app"),
            PathBuf::from("/Applications/hgs-tray.app"),
        ] {
            frontend_images.push(bundle.join("Contents/MacOS/hgs-tray"));
        }
        #[cfg(target_os = "linux")]
        frontend_images.push(PathBuf::from("/usr/bin/hgs-tray"));
        Self {
            location: Location {
                root: location.root.clone(),
                configuration: location.configuration.clone(),
            },
            frontend_images,
        }
    }
    fn selected_frontend(&self, image: &Path) -> bool {
        self.installed_frontend(image).is_some()
    }
    pub(super) fn installed_frontend(&self, image: &Path) -> Option<&Path> {
        let identity = platform::file_identity(image)?;
        self.frontend_images
            .iter()
            .find(|path| platform::file_identity(path) == Some(identity))
            .map(PathBuf::as_path)
    }
    /// Bind an explicit maintained-image choice; this never discovers one from
    /// PATH or turns an inbound path check into a launch/worker permission.
    #[cfg(target_os = "linux")]
    #[allow(dead_code)] // Consumed by the following Linux UI-closed launch slice.
    pub fn select_frontend(
        &self,
        image: &Path,
    ) -> Result<super::notifications::frontend::SelectedFrontend> {
        super::notifications::frontend::SelectedFrontend::select(self, image)
    }
}

#[derive(Clone)]
struct Process {
    pid: u32,
    identity: platform::ProcessIdentity,
    executable: String,
    #[cfg(windows)]
    original: Option<std::sync::Arc<Original>>,
}
impl Process {
    #[cfg(windows)]
    fn read(pid: u32) -> Result<Self> {
        let original = std::sync::Arc::new(Original::open(pid)?);
        ensure!(original.own, "frontend belongs to another user");
        Ok(Self {
            pid,
            identity: platform::ProcessIdentity {
                parent: original.parent,
                group: None,
                birth: original.birth.to_string(),
                started: (original.birth as f64 - 116444736000000000f64) / 10000000f64,
            },
            executable: original.image.clone(),
            original: Some(original),
        })
    }
    #[cfg(not(windows))]
    fn read(pid: u32) -> Result<Self> {
        let identity =
            platform::process_identity(pid).context("frontend process identity unavailable")?;
        let executable = platform::describe(pid)
            .context("frontend process image unavailable")?
            .executable;
        ensure!(
            platform::process_identity(pid).as_ref() == Some(&identity),
            "frontend process changed"
        );
        Ok(Self {
            pid,
            identity,
            executable,
        })
    }
    #[cfg(windows)]
    fn current(&self) -> bool {
        self.original
            .as_ref()
            .is_some_and(|original| original.current())
    }
    #[cfg(not(windows))]
    fn current(&self) -> bool {
        platform::process_identity(self.pid).as_ref() == Some(&self.identity)
            && platform::describe(self.pid).is_some_and(|p| p.executable == self.executable)
    }
    #[cfg(windows)]
    fn arguments(&self) -> Result<Vec<String>> {
        Ok(self
            .original
            .as_ref()
            .context("original frontend process unavailable")?
            .arguments()?)
    }
    #[cfg(not(windows))]
    fn arguments(&self) -> Result<Vec<String>> {
        platform::describe(self.pid)
            .and_then(|p| p.argv)
            .context("frontend argv unavailable")
    }
}

#[cfg(windows)]
struct WindowsFreshness {
    canonical: Peer,
    ancestry: Ancestry,
}
#[cfg(windows)]
impl WindowsFreshness {
    fn check(&self, until: Instant) -> Result<()> {
        ensure!(
            self.canonical.current() && Instant::now() < until,
            "canonical frontend expired"
        );
        self.ancestry.validate(until)?;
        ensure!(
            self.canonical.current() && Instant::now() < until,
            "canonical frontend expired"
        );
        Ok(())
    }
}

/// Opaque and one-use; only authenticate() can construct it. The actor consumes
/// it just before its durable mutation. No wire decoder can construct authority.
pub(super) struct Authorized {
    envelope: Envelope,
    caller: Process,
    frontend: Process,
    until: Instant,
    instance: String,
    serial: u64,
    ownership_origin: Option<super::ownership::Origin>,
    notification: Option<zerus_client_core::desktop::notification_control::NativeControl>,
    #[cfg(windows)]
    windows: Option<Box<WindowsFreshness>>,
}
/// Actor-side permit after the original peer was revalidated. It deliberately
/// has no Clone/Deserialize or public constructor, even inside the request lane.
#[derive(Default)]
struct Metadata {
    ownership: Option<super::ownership::Origin>,
    notification: Option<zerus_client_core::desktop::notification_control::NativeControl>,
}
pub(super) struct Validated(Request, Freshness, Metadata);
enum Freshness {
    Native {
        caller: Process,
        frontend: Process,
        until: Instant,
        #[cfg(windows)]
        windows: Option<Box<WindowsFreshness>>,
    },
    #[cfg(test)]
    Fixture,
}
impl Validated {
    pub fn id(&self) -> &str {
        self.0.id()
    }
    pub fn operation(&self) -> &'static str {
        self.0.operation()
    }
    fn check(&self) -> Result<()> {
        match &self.1 {
            Freshness::Native {
                caller,
                frontend,
                until,
                #[cfg(windows)]
                windows,
            } => {
                ensure!(
                    Instant::now() < *until && caller.current() && frontend.current(),
                    "frontend permit expired"
                );
                #[cfg(windows)]
                windows
                    .as_ref()
                    .context("original Windows evidence missing")?
                    .check(*until)?;
            }
            #[cfg(test)]
            Freshness::Fixture => (),
        }
        Ok(())
    }
    pub fn into_request(self) -> Result<Request> {
        self.check()?;
        ensure!(
            !matches!(
                self.0,
                Request::DesktopOwnership { .. }
                    | Request::NotificationControl { .. }
                    | Request::CacheMaintenance { .. }
                    | Request::CreationClone { .. }
            ),
            "native controls require their typed boundary"
        );
        Ok(self.0)
    }
    /// Only the clone actor calls this after BEGIN IMMEDIATE. Cached native
    /// Service mode was bound to this exact original request during admission.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn creation_clone(self) -> Result<zerus_client_core::desktop::creation_clone::Intent> {
        self.check()?;
        let Request::CreationClone { intent } = self.0 else {
            anyhow::bail!("not a creation clone");
        };
        Ok(*intent)
    }
    pub fn ownership_parts(&self) -> Result<(Request, super::ownership::Origin)> {
        self.check()?;
        ensure!(
            matches!(self.0, Request::DesktopOwnership { .. }),
            "not ownership"
        );
        let origin = self
            .2
            .ownership
            .as_ref()
            .context("original ownership identity missing")?;
        ensure!(origin.current(), "ownership image changed");
        Ok((self.0.clone(), origin.clone()))
    }
    pub fn check_ownership(self) -> Result<()> {
        self.ownership_parts().map(|_| ())
    }
    pub fn cache_parts(
        &self,
    ) -> Result<(
        String,
        zerus_client_core::desktop::cache_maintenance::Command,
    )> {
        self.check()?;
        let Request::CacheMaintenance {
            request_id,
            command,
        } = &self.0
        else {
            anyhow::bail!("not cache maintenance");
        };
        ensure!(command.valid(request_id), "invalid cache maintenance");
        Ok((request_id.clone(), *command.clone()))
    }
    pub fn check_cache(self) -> Result<()> {
        self.cache_parts().map(|_| ())
    }
    #[cfg(test)]
    pub fn fixture(value: &Value) -> Self {
        Self(
            serde_json::from_value(value.clone()).expect("synthetic native-policy fixture"),
            Freshness::Fixture,
            Metadata::default(),
        )
    }
}
/// A read-only activation receiver bound to the original authenticated child.
/// It cannot authorize policy mutations and cannot be decoded from JSON.
pub(super) struct ActivationReceiver {
    freshness: Freshness,
    until: Instant,
    next_full_check: Cell<Instant>,
    #[cfg(test)]
    probe: Option<std::sync::Arc<ActivationProbe>>,
}
impl Freshness {
    // Held caller/frontend and canonical-peer handles, without ancestry or
    // environment reads. Missing Windows evidence still refuses every turn.
    fn activation_live(&self, until: Instant) -> bool {
        if Instant::now() >= until {
            return false;
        }
        match self {
            Freshness::Native {
                caller,
                frontend,
                #[cfg(windows)]
                windows,
                ..
            } => {
                if !caller.current() || !frontend.current() {
                    return false;
                }
                #[cfg(windows)]
                if windows
                    .as_ref()
                    .is_none_or(|proof| !proof.canonical.current())
                {
                    return false;
                }
                Instant::now() < until
            }
            #[cfg(test)]
            Freshness::Fixture => true,
        }
    }
    // Full read-only proof checks never alter the mutation permit's `until`.
    fn activation_current(&self, until: Instant) -> bool {
        if !self.activation_live(until) {
            return false;
        }
        #[cfg(windows)]
        match self {
            Freshness::Native { windows, .. } => {
                if windows
                    .as_ref()
                    .is_none_or(|proof| proof.check(until).is_err())
                {
                    return false;
                }
            }
            #[cfg(test)]
            Freshness::Fixture => (),
        }
        Instant::now() < until
    }
}
impl ActivationReceiver {
    fn new(freshness: Freshness, until: Instant) -> Self {
        Self {
            freshness,
            until,
            // Admission just validated the full original proof.
            next_full_check: Cell::new(Instant::now() + ACTIVATION_PROOF_INTERVAL),
            #[cfg(test)]
            probe: None,
        }
    }
    fn live(&self) -> bool {
        #[cfg(test)]
        if let Some(probe) = &self.probe {
            probe
                .live_checks
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return Instant::now() < self.until
                && probe.live.load(std::sync::atomic::Ordering::SeqCst);
        }
        self.freshness.activation_live(self.until)
    }
    pub fn current(&self) -> bool {
        if !self.live() {
            return false;
        }
        Instant::now() < self.next_full_check.get() || self.handoff_current()
    }
    /// Every actual click handoff bypasses the periodic proof cache.
    pub fn handoff_current(&self) -> bool {
        if !self.live() {
            return false;
        }
        let until = self.until.min(Instant::now() + WAIT);
        #[cfg(test)]
        let valid = if let Some(probe) = &self.probe {
            probe
                .full_checks
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            probe.valid.load(std::sync::atomic::Ordering::SeqCst)
        } else {
            self.freshness.activation_current(until)
        };
        #[cfg(not(test))]
        let valid = self.freshness.activation_current(until);
        if valid {
            self.next_full_check
                .set(Instant::now() + ACTIVATION_PROOF_INTERVAL);
        }
        valid
    }
    #[cfg(test)]
    pub fn set_probe(&mut self, probe: std::sync::Arc<ActivationProbe>, next_full_check: Instant) {
        self.probe = Some(probe);
        self.next_full_check.set(next_full_check);
    }
}
#[cfg(test)]
pub(super) struct ActivationProbe {
    pub live: std::sync::atomic::AtomicBool,
    pub valid: std::sync::atomic::AtomicBool,
    pub live_checks: std::sync::atomic::AtomicUsize,
    pub full_checks: std::sync::atomic::AtomicUsize,
}
#[cfg(test)]
impl Default for ActivationProbe {
    fn default() -> Self {
        Self {
            live: std::sync::atomic::AtomicBool::new(true),
            valid: std::sync::atomic::AtomicBool::new(true),
            live_checks: std::sync::atomic::AtomicUsize::new(0),
            full_checks: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}
impl Validated {
    pub fn activation_wait(self) -> Result<(String, ActivationReceiver)> {
        let Self(request, freshness, _) = self;
        let original_until = match &freshness {
            Freshness::Native { until, .. } => *until,
            #[cfg(test)]
            Freshness::Fixture => Instant::now() + WAIT,
        };
        ensure!(
            freshness.activation_current(original_until),
            "frontend permit expired"
        );
        let Request::ActivationWait { request_id } = request else {
            anyhow::bail!("not an activation receiver")
        };
        Ok((
            request_id,
            ActivationReceiver::new(
                freshness,
                Instant::now() + zerus_client_core::desktop::activation::PEER_WAIT,
            ),
        ))
    }
}
impl Authorized {
    pub fn operation(&self) -> &'static str {
        self.envelope.request.operation()
    }
    pub fn into_request(self, order: &mut Order) -> Result<Validated> {
        ensure!(
            Instant::now() < self.until && self.caller.current() && self.frontend.current(),
            "frontend request identity expired"
        );
        #[cfg(windows)]
        self.windows
            .as_ref()
            .context("original Windows evidence missing")?
            .check(self.until)?;
        order.observe(
            (
                self.frontend.pid,
                self.frontend.identity.birth.clone(),
                self.instance,
            ),
            OrderedOperation::from(&self.envelope.request),
            self.serial,
            Instant::now(),
        )?;
        Ok(Validated(
            self.envelope.request,
            Freshness::Native {
                caller: self.caller,
                frontend: self.frontend,
                until: self.until,
                #[cfg(windows)]
                windows: self.windows,
            },
            Metadata {
                ownership: self.ownership_origin,
                notification: self.notification,
            },
        ))
    }
}

pub(super) fn authenticate(location: &Scope, peer_pid: u32, value: &Value) -> Result<Authorized> {
    let envelope: Envelope =
        serde_json::from_value(value.clone()).context("invalid frontend envelope")?;
    ensure!(envelope.valid(), "invalid frontend envelope");
    let until = Instant::now() + WAIT;
    let caller = Process::read(peer_pid)?;
    let own_image = platform::describe(std::process::id()).context("service image unavailable")?;
    let selected = platform::file_identity(Path::new(&own_image.executable))
        .context("service image identity unavailable")?;
    ensure!(
        platform::file_identity(Path::new(&caller.executable)) == Some(selected),
        "frontend relay is not the selected hgs"
    );
    let argv = caller.arguments()?;
    ensure!(
        argv.get(1..)
            .is_some_and(|a| a == ["desktop", "frontend-request", "--json"]),
        "unexpected frontend relay"
    );
    let Canonical {
        pid: frontend_pid,
        reply,
        #[cfg(windows)]
        peer,
    } = probe(location, &envelope.attempt, until)
        .context("canonical frontend evidence unavailable")?;
    let frontend = Process::read(frontend_pid)?;
    ensure!(
        location.selected_frontend(Path::new(&frontend.executable)),
        "canonical frontend is not the installed application"
    );
    validate(
        &envelope,
        &caller,
        &frontend,
        &reply,
        &location.configuration,
    )?;
    #[cfg(not(windows))]
    check_ancestry(peer_pid, until)?;
    #[cfg(windows)]
    let windows = {
        ensure!(
            peer.current() && peer.birth == frontend.identity.birth,
            "canonical process was replaced"
        );
        let windows = WindowsFreshness {
            canonical: peer,
            ancestry: Ancestry::inspect(peer_pid, until)?,
        };
        windows.check(until)?;
        windows
    };
    ensure!(
        Instant::now() < until && caller.current() && frontend.current(),
        "frontend changed during authentication"
    );
    let ownership_origin = if matches!(envelope.request, Request::DesktopOwnership { .. }) {
        Some(super::ownership::Origin::capture(
            Path::new(&frontend.executable),
            reply.hello.build.clone(),
        )?)
    } else {
        None
    };
    Ok(Authorized {
        envelope,
        caller,
        frontend,
        until,
        instance: reply.hello.instance_epoch,
        serial: reply.request.serial,
        ownership_origin,
        notification: reply.request.notification,
        #[cfg(windows)]
        windows: Some(Box::new(windows)),
    })
}

fn validate(
    envelope: &Envelope,
    caller: &Process,
    frontend: &Process,
    reply: &Reply,
    configuration: &str,
) -> Result<()> {
    ensure!(
        reply
            .hello
            .identity_matches(&envelope.attempt, configuration),
        "frontend identity/configuration does not match"
    );
    ensure!(
        caller.identity.parent == frontend.pid,
        "relay is not a direct frontend child"
    );
    let image = Path::new(&frontend.executable)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    ensure!(
        matches!(
            image,
            "zerus-client"
                | "zerus-next"
                | "hgs-tray"
                | "Zerus"
                | "zerus-client.exe"
                | "zerus-next.exe"
                | "hgs-tray.exe"
                | "Zerus.exe"
        ),
        "canonical peer is not a native frontend"
    );
    ensure!(
        reply.request.serial != 0
            && reply.request.attempt == envelope.attempt
            && reply.request.child_pid == caller.pid
            && reply.request.child_birth == caller.identity.birth
            && reply.request.request_id == envelope.request.id()
            && reply.request.fingerprint == envelope.request.fingerprint(),
        "original frontend child/request does not match"
    );
    if let Request::DesktopOwnership { action, .. } = &envelope.request {
        ensure!(
            reply.request.ownership_mode == Some(action.mode()),
            "ownership mode does not match the native frontend"
        );
    }
    if let Request::CacheMaintenance { command, .. } = &envelope.request {
        let selected = match command.as_ref() {
            zerus_client_core::desktop::cache_maintenance::Command::Prepare {
                configuration,
                ..
            }
            | zerus_client_core::desktop::cache_maintenance::Command::Execute {
                configuration,
                ..
            } => configuration,
        };
        ensure!(
            reply.request.ownership_mode
                == Some(zerus_client_core::desktop::ownership::Mode::Service)
                && selected == configuration,
            "cache mode/configuration does not match the native frontend"
        );
    }
    if let Request::CreationClone { intent } = &envelope.request {
        ensure!(
            reply.request.ownership_mode
                == Some(zerus_client_core::desktop::ownership::Mode::Service)
                && intent.scope.configuration == configuration,
            "creation does not match the native service frontend"
        );
    }
    if let Request::NotificationControl { scope, control } = &envelope.request {
        let native = reply
            .request
            .notification
            .as_ref()
            .context("native notification evidence missing")?;
        ensure!(
            native.scope == *scope
                && native.scope.configuration == configuration
                && native.mode == zerus_client_core::desktop::ownership::Mode::Service
                && match control {
                    zerus_client_core::desktop::notification_control::Command::Configure {
                        ..
                    } => native.focus.is_none(),
                    zerus_client_core::desktop::notification_control::Command::Focus { .. } =>
                        native.focus.as_ref().is_some_and(|v| v.valid()),
                },
            "native notification scope/mode does not match"
        );
    } else {
        ensure!(
            reply.request.notification.is_none(),
            "unexpected native notification evidence"
        );
    }
    if let Request::MaintenanceConfigure {
        update: Some(update),
        ..
    } = &envelope.request
    {
        ensure!(
            update.gui.bundle_id == reply.hello.application
                && update.gui.app_version == reply.hello.build.version
                && Some(update.gui.dev_number) == dev_number(&reply.hello.build.version)
                && update.gui.source_commit == reply.hello.build.source_commit
                && platform::file_identity(Path::new(&update.gui.executable)).is_some_and(
                    |id| Some(id) == platform::file_identity(Path::new(&frontend.executable))
                ),
            "maintenance GUI origin does not match the authenticated frontend"
        );
    }
    Ok(())
}

fn dev_number(version: &str) -> Option<u64> {
    match version.split_once('-').map(|(_, prerelease)| prerelease) {
        Some(prerelease) if prerelease.starts_with("dev.") => prerelease[4..]
            .parse::<u64>()
            .ok()
            .filter(|number| *number > 0 && number.to_string() == prerelease[4..]),
        _ => Some(0),
    }
}

// Exhaustively map the closed native request DTO. A new Request variant must
// choose its ordering lane here before it can compile; there is no second
// string allowlist that can silently reject an otherwise authenticated read.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum OrderedOperation {
    CacheMaintenance,
    CreationClone,
    ActivationWait,
    AutomaticApprovals,
    MaintenanceConfigure,
    ImportPendingReads,
    DesktopOwnership,
    NotificationPreferences,
    NotificationFocus,
}
impl From<&Request> for OrderedOperation {
    fn from(request: &Request) -> Self {
        use zerus_client_core::desktop::notification_control::Command;
        match request {
            Request::CacheMaintenance { .. } => Self::CacheMaintenance,
            Request::CreationClone { .. } => Self::CreationClone,
            Request::ActivationWait { .. } => Self::ActivationWait,
            Request::AutomaticApprovals { .. } => Self::AutomaticApprovals,
            Request::MaintenanceConfigure { .. } => Self::MaintenanceConfigure,
            Request::ImportPendingReads { .. } => Self::ImportPendingReads,
            Request::DesktopOwnership { .. } => Self::DesktopOwnership,
            Request::NotificationControl { control, .. } => match control {
                Command::Configure { .. } => Self::NotificationPreferences,
                Command::Focus { .. } => Self::NotificationFocus,
            },
        }
    }
}

type Instance = (u32, String, String);
#[derive(Default)]
pub(super) struct Order {
    current: Option<(Instance, BTreeMap<OrderedOperation, u64>)>,
    retired: BTreeMap<Instance, Instant>,
}
impl Order {
    /// Intent order comes from the native cache, never a caller field. This
    /// prevents a slower earlier Enable from overtaking a later Disable.
    fn observe(
        &mut self,
        instance: Instance,
        operation: OrderedOperation,
        serial: u64,
        now: Instant,
    ) -> Result<()> {
        self.retired.retain(|_, until| now < *until);
        ensure!(
            serial != 0 && !self.retired.contains_key(&instance),
            "stale frontend instance"
        );
        if let Some((old, previous)) = &mut self.current {
            if *old == instance {
                ensure!(
                    serial > previous.get(&operation).copied().unwrap_or(0),
                    "stale frontend intent"
                );
                previous.insert(operation, serial);
                return Ok(());
            } else {
                ensure!(
                    self.retired.len() < 16,
                    "frontend instance changes are busy"
                );
                self.retired.insert(old.clone(), now + WAIT);
            }
        }
        self.current = Some((instance, BTreeMap::from([(operation, serial)])));
        Ok(())
    }
}

struct Canonical {
    pid: u32,
    reply: Reply,
    #[cfg(windows)]
    peer: Peer,
}
#[cfg(unix)]
fn probe(location: &Location, attempt: &str, _until: Instant) -> Option<Canonical> {
    let (pid, reply) = zerus_client_core::desktop::probe_ui_request_at(
        &owner::instance_path(),
        attempt,
        &location.configuration,
    )?;
    Some(Canonical { pid, reply })
}
#[cfg(windows)]
fn probe(location: &Location, attempt: &str, until: Instant) -> Option<Canonical> {
    let name =
        zerus_client_core::desktop::windows::activation_name("hgs-tray", &std::env::home_dir()?);
    let (peer, reply) = zerus_client_core::desktop::windows::probe_ui_request_at(
        &name,
        attempt,
        &location.configuration,
        until,
    )?;
    Some(Canonical {
        pid: peer.pid,
        peer,
        reply,
    })
}
#[cfg(not(any(unix, windows)))]
fn probe(_: &Location, _: &str, _: Instant) -> Option<Canonical> {
    None
}

#[cfg(target_os = "macos")]
fn check_ancestry(pid: u32, until: Instant) -> Result<()> {
    macos_ancestry::inspect(pid, until).map(|_| ())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn check_ancestry(pid: u32, until: Instant) -> Result<()> {
    ancestry(pid, until, |pid| {
        if !same_user(pid)? {
            return Ok(None);
        }
        #[cfg(target_os = "linux")]
        if linux_manager::boundary(pid, unsafe { libc::geteuid() }) {
            return Ok(None);
        }
        let process = Process::read(pid)?;
        #[cfg(target_os = "linux")]
        untraced(pid)?;
        let tagged = agent_environment(pid)?;
        let agent = tagged || agent_image(&process.executable);
        ensure!(process.current(), "frontend ancestry changed");
        Ok(Some((process.identity.parent, agent)))
    })
}

#[cfg(any(not(windows), test))]
fn agent_image(image: &str) -> bool {
    let name = Path::new(image)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .trim_end_matches(".exe");
    matches!(
        name,
        "codex" | "claude" | "deepseek" | "gemini" | "opencode"
    )
}

#[cfg(any(all(not(windows), not(target_os = "macos")), test))]
fn ancestry(
    mut pid: u32,
    until: Instant,
    mut inspect: impl FnMut(u32) -> Result<Option<(u32, bool)>>,
) -> Result<()> {
    let mut seen = BTreeSet::new();
    while pid != 0 {
        ensure!(
            seen.len() < ANCESTORS && Instant::now() < until && seen.insert(pid),
            "frontend ancestry unavailable"
        );
        let Some((parent, agent)) = inspect(pid)? else {
            return Ok(());
        };
        ensure!(!agent, "agent-descended frontend request refused");
        pid = parent;
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn marked(bytes: &[u8]) -> bool {
    bytes.split(|b| *b == 0).any(|entry| {
        [
            b"HGS_SESSION=".as_slice(),
            b"HGS_RUN_ID=",
            b"LD_PRELOAD=",
            b"LD_AUDIT=",
            b"DYLD_INSERT_LIBRARIES=",
            b"DYLD_FORCE_FLAT_NAMESPACE=",
        ]
        .iter()
        .any(|key| entry.starts_with(key))
    })
}

#[cfg(any(target_os = "linux", test))]
fn tracer_absent(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut fields = text
        .lines()
        .filter_map(|line| line.strip_prefix("TracerPid:"));
    fields
        .next()
        .is_some_and(|v| v.trim().parse::<u32>() == Ok(0))
        && fields.next().is_none()
}
#[cfg(target_os = "linux")]
fn untraced(pid: u32) -> Result<()> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(format!("/proc/{pid}/status"))?;
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 65536 && tracer_absent(&bytes),
        "traced frontend ancestry refused"
    );
    Ok(())
}
#[cfg(target_os = "linux")]
fn same_user(pid: u32) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    Ok(std::fs::metadata(format!("/proc/{pid}"))?.uid() == unsafe { libc::geteuid() })
}
#[cfg(target_os = "linux")]
fn agent_environment(pid: u32) -> Result<bool> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(format!("/proc/{pid}/environ"))?;
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 65536 && (bytes.is_empty() || bytes.last() == Some(&0)),
        "frontend environment unavailable"
    );
    Ok(marked(&bytes))
}

#[cfg(target_os = "macos")]
fn agent_environment(pid: u32) -> Result<bool> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as i32];
    let mut bytes = vec![0u8; 65536];
    let mut size = bytes.len();
    // SAFETY: fixed MIB and initialized writable buffer with its capacity.
    let code = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    ensure!(
        code == 0 && (4..=bytes.len()).contains(&size),
        "frontend environment unavailable"
    );
    bytes.truncate(size);
    let argc = i32::from_ne_bytes(bytes[..4].try_into()?);
    ensure!((1..=4096).contains(&argc), "frontend argv unavailable");
    let mut rest = &bytes[4..];
    rest = &rest[rest
        .iter()
        .position(|b| *b == 0)
        .context("invalid executable")?
        + 1..];
    while rest.first() == Some(&0) {
        rest = &rest[1..];
    }
    for _ in 0..argc {
        rest = &rest[rest.iter().position(|b| *b == 0).context("invalid argv")? + 1..];
    }
    Ok(marked(rest))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn same_user(_: u32) -> Result<bool> {
    anyhow::bail!("frontend ancestry is unavailable on this platform")
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn agent_environment(_: u32) -> Result<bool> {
    anyhow::bail!("frontend ancestry is unavailable on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerus_client_core::desktop::{
        ui_requests::{Evidence, Request, ABI},
        Build, Hello,
    };

    fn id(n: usize) -> String {
        format!("00000000-0000-4000-8000-{n:012x}")
    }
    fn fixture() -> (Envelope, Process, Process, Reply) {
        let envelope = Envelope {
            abi: ABI.into(),
            attempt: id(1),
            request: Request::AutomaticApprovals {
                request_id: id(2),
                enabled: true,
            },
        };
        let frontend = Process {
            pid: 7,
            identity: platform::ProcessIdentity {
                parent: 1,
                group: None,
                birth: "100".into(),
                started: 100.0,
            },
            executable: "/opt/example/zerus-client".into(),
            #[cfg(windows)]
            original: None,
        };
        let caller = Process {
            pid: 8,
            identity: platform::ProcessIdentity {
                parent: 7,
                group: None,
                birth: "101".into(),
                started: 101.0,
            },
            executable: "/opt/example/hgs".into(),
            #[cfg(windows)]
            original: None,
        };
        let reply = Reply {
            hello: Hello {
                abi: zerus_client_core::desktop::ABI.into(),
                version: 1,
                nonce: id(1),
                application: "com.hgdev.hgs-tray".into(),
                gui: "tauri".into(),
                build: Build {
                    version: "1.0.0".into(),
                    source_commit: "a".repeat(40),
                    source_dirty: Some(false),
                },
                instance_epoch: id(3),
                writer_epoch: None,
                configuration: Some("a".repeat(64)),
                live: true,
                replay: false,
            },
            request: Evidence {
                ownership_mode: None,
                notification: None,
                attempt: id(1),
                request_id: id(2),
                fingerprint: envelope.request.fingerprint(),
                child_pid: 8,
                child_birth: "101".into(),
                serial: 1,
            },
        };
        (envelope, caller, frontend, reply)
    }

    #[test]
    fn original_child_and_request_join_is_closed() {
        let config = "a".repeat(64);
        let (request, caller, frontend, reply) = fixture();
        validate(&request, &caller, &frontend, &reply, &config).unwrap();
        for changed in 0..10 {
            let (request, mut caller, mut frontend, mut reply) = fixture();
            match changed {
                0 => reply.request.child_pid += 1,
                1 => reply.request.child_birth = "reused".into(),
                2 => reply.request.fingerprint = "b".repeat(64),
                3 => reply.request.request_id = id(4),
                4 => reply.request.attempt = id(4),
                5 => caller.identity.parent = 99,
                6 => frontend.executable = "/bin/sh".into(),
                7 => reply.hello.gui = "qt".into(),
                8 => reply.hello.configuration = Some("b".repeat(64)),
                _ => reply.request.serial = 0,
            }
            assert!(validate(&request, &caller, &frontend, &reply, &config).is_err());
        }
    }

    #[test]
    fn creation_clone_requires_cached_service_mode_and_exact_configuration_at_the_typed_boundary() {
        use zerus_client_core::desktop::{
            creation_clone::{Intent, Scope},
            ownership::Mode,
        };
        let (mut envelope, caller, frontend, mut reply) = fixture();
        let original = serde_json::json!({"id":id(80),"creation_schema":1,"created":1,"revision":4,
            "cmd":"codex","tag":"example","host":"","state":"cloning","repository":true,
            "parent":"/srv","target":"/srv/repo","repository_name":"repo",
            "url":"https://example.invalid/repo.git","clone_id":id(2),"expected_machine":id(81)});
        envelope.request = Request::CreationClone {
            intent: Box::new(Intent {
                scope: Scope {
                    configuration: "a".repeat(64),
                    service_epoch: id(91),
                },
                creation_id: id(80),
                clone_request_id: id(2),
                revision: 4,
                original: original.to_string(),
            }),
        };
        assert_eq!(
            OrderedOperation::from(&envelope.request),
            OrderedOperation::CreationClone
        );
        #[cfg(unix)]
        {
            // This isolates ordering after authentication with actual kernel
            // currentness; it is not a native clone admission/execution proof.
            let mut order = Order::default();
            live_authorized(envelope.request.clone(), 7)
                .into_request(&mut order)
                .unwrap()
                .creation_clone()
                .unwrap();
            live_authorized(Request::ActivationWait { request_id: id(82) }, 1)
                .into_request(&mut order)
                .unwrap()
                .activation_wait()
                .unwrap();
            assert!(live_authorized(envelope.request.clone(), 7)
                .into_request(&mut order)
                .is_err());
            live_authorized(envelope.request.clone(), 8)
                .into_request(&mut order)
                .unwrap()
                .creation_clone()
                .unwrap();
        }
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Native);
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Service);
        validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).unwrap();
        assert!(Validated::fixture(&serde_json::json!(envelope.request))
            .into_request()
            .is_err());
        if let Request::CreationClone { intent } = &mut envelope.request {
            intent.scope.configuration = "b".repeat(64);
        }
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
    }
    #[test]
    fn notification_controls_require_cached_service_mode_and_matching_native_scope() {
        use zerus_client_core::desktop::notification_control::{
            Command, FocusState, NativeControl, NativeFocus, Scope,
        };
        let (mut envelope, caller, frontend, mut reply) = fixture();
        let scope = Scope {
            configuration: "a".repeat(64),
            service_epoch: id(91),
        };
        envelope.request = Request::NotificationControl {
            scope: scope.clone(),
            control: Command::Focus { request_id: id(2) },
        };
        reply.request.fingerprint = envelope.request.fingerprint();
        let native = NativeControl {
            scope,
            mode: zerus_client_core::desktop::ownership::Mode::Service,
            focus: Some(NativeFocus {
                connection_id: id(92),
                revision: 1,
                state: FocusState::Foreground,
            }),
        };
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.notification = Some(native.clone());
        validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).unwrap();
        for changed in 0..5 {
            let mut bad = native.clone();
            match changed {
                0 => bad.mode = zerus_client_core::desktop::ownership::Mode::Native,
                1 => bad.scope.service_epoch = id(93),
                2 => bad.scope.configuration = "b".repeat(64),
                3 => bad.focus = None,
                _ => bad.focus.as_mut().unwrap().revision = 0,
            }
            reply.request.notification = Some(bad);
            assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        }
        let permit = Validated::notification_fixture(
            native.scope,
            Command::Focus { request_id: id(94) },
            native.focus,
        );
        assert!(
            permit.into_request().is_err(),
            "generic mutation consumes no notification authority"
        );
    }
    #[test]
    fn native_notification_operations_are_admitted_and_ordered_independently() {
        let mut order = Order::default();
        let instance = (42, "birth".to_string(), id(95));
        let now = Instant::now();
        order
            .observe(
                instance.clone(),
                OrderedOperation::NotificationPreferences,
                2,
                now,
            )
            .unwrap();
        order
            .observe(
                instance.clone(),
                OrderedOperation::NotificationFocus,
                1,
                now,
            )
            .unwrap();
        assert!(order
            .observe(
                instance.clone(),
                OrderedOperation::NotificationPreferences,
                1,
                now
            )
            .is_err());
        assert!(order
            .observe(
                instance.clone(),
                OrderedOperation::NotificationFocus,
                1,
                now
            )
            .is_err());
        order
            .observe(instance, OrderedOperation::NotificationFocus, 2, now)
            .unwrap();
    }

    #[test]
    fn cache_native_mode_and_selected_configuration_cannot_be_claimed_by_the_request() {
        use zerus_client_core::desktop::{
            cache_maintenance::{Command, Timing},
            ownership::Mode,
        };
        let (mut request, caller, frontend, mut reply) = fixture();
        request.request = Request::CacheMaintenance {
            request_id: id(2),
            command: Box::new(Command::Prepare {
                operation_id: id(10),
                configuration: "a".repeat(64),
                root: "/srv/example/cache".into(),
                limit: 1,
                revision: 0,
                timing: Timing::Immediate,
                policy_change: true,
            }),
        };
        reply.request.fingerprint = request.request.fingerprint();
        for mode in [None, Some(Mode::Native), Some(Mode::Service)] {
            reply.request.ownership_mode = mode;
            assert_eq!(
                validate(&request, &caller, &frontend, &reply, &"a".repeat(64)).is_ok(),
                mode == Some(Mode::Service)
            );
        }
        if let Request::CacheMaintenance { command, .. } = &mut request.request {
            if let Command::Prepare { configuration, .. } = command.as_mut() {
                *configuration = "b".repeat(64);
            }
        }
        reply.request.fingerprint = request.request.fingerprint();
        assert!(validate(&request, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
    }

    #[test]
    fn ownership_requires_the_original_cached_native_mode_in_addition_to_child_proof() {
        use zerus_client_core::desktop::ownership::{Action, Mode};
        for action in [Action::Grant, Action::Rearm, Action::NativeTakeover] {
            let (mut request, caller, frontend, mut reply) = fixture();
            request.request = Request::DesktopOwnership {
                request_id: id(2),
                action,
                journal: None,
                revision: 0,
            };
            reply.request.fingerprint = request.request.fingerprint();
            for mode in [None, Some(Mode::Native), Some(Mode::Service)] {
                reply.request.ownership_mode = mode;
                assert_eq!(
                    validate(&request, &caller, &frontend, &reply, &"a".repeat(64)).is_ok(),
                    mode == Some(action.mode()),
                );
            }
            reply.request.ownership_mode = Some(action.mode());
            reply.request.child_birth = "wrong original child".into();
            assert!(validate(&request, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        }
    }

    #[test]
    fn a_retained_permit_must_still_be_fresh_at_the_mutation_boundary() {
        let (envelope, caller, frontend, _) = fixture();
        let permit = Validated(
            envelope.request,
            Freshness::Native {
                caller,
                frontend,
                until: Instant::now(),
                #[cfg(windows)]
                windows: None,
            },
            Metadata::default(),
        );
        // Expiration refuses before attempting any lookup of these fake PIDs.
        assert!(permit.into_request().is_err());
    }

    #[test]
    fn maintenance_gui_identity_is_bound_to_the_actual_frontend_not_the_cli() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("hgs-tray");
        std::fs::write(&path, b"private fixture image").unwrap();
        let (mut envelope, caller, mut frontend, mut reply) = fixture();
        frontend.executable = path.to_str().unwrap().into();
        let update = zerus_client_core::maintenance::UpdateConfiguration {
            gui: zerus_client_core::maintenance::GuiIdentity {
                bundle_id: reply.hello.application.clone(),
                executable: frontend.executable.clone(),
                app_version: reply.hello.build.version.clone(),
                dev_number: 0,
                source_commit: reply.hello.build.source_commit.clone(),
            },
            channel: zerus_client_core::updates::Channel::Stable,
            automatic: true,
        };
        envelope.request = Request::MaintenanceConfigure {
            request_id: id(2),
            update: Some(Box::new(update)),
            recovery: None,
        };
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_ok());
        for field in 0..4 {
            let mut wrong = envelope.clone();
            let Request::MaintenanceConfigure {
                update: Some(update),
                ..
            } = &mut wrong.request
            else {
                unreachable!()
            };
            match field {
                0 => update.gui.app_version = "another build".into(),
                1 => update.gui.source_commit = "b".repeat(40),
                2 => update.gui.dev_number = 42,
                _ => {
                    update.gui.executable = temp
                        .path()
                        .join("not-the-frontend")
                        .to_str()
                        .unwrap()
                        .into()
                }
            }
            reply.request.fingerprint = wrong.request.fingerprint();
            assert!(validate(&wrong, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        }
        reply.hello.build.version = "1.0.0-dev.42".into();
        let Request::MaintenanceConfigure {
            update: Some(update),
            ..
        } = &mut envelope.request
        else {
            unreachable!()
        };
        update
            .gui
            .app_version
            .clone_from(&reply.hello.build.version);
        for number in [0, 41, 42, 43] {
            let Request::MaintenanceConfigure {
                update: Some(update),
                ..
            } = &mut envelope.request
            else {
                unreachable!()
            };
            update.gui.dev_number = number;
            reply.request.fingerprint = envelope.request.fingerprint();
            assert_eq!(
                validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_ok(),
                number == 42
            );
        }
    }

    #[test]
    fn dev_number_uses_only_the_bound_version_suffix() {
        assert_eq!(dev_number("1.0.0"), Some(0));
        assert_eq!(dev_number("1.0.0-preview.fixture.abcdef0"), Some(0));
        assert_eq!(dev_number("0.48.0-preview.local-dev.1a2b3c4"), Some(0));
        assert_eq!(dev_number("0.48.0-preview.local-dev.1234567"), Some(0));
        assert_eq!(dev_number("1.0.0-dev.42"), Some(42));
        for version in [
            "1.0.0-dev.",
            "1.0.0-dev.0",
            "1.0.0-dev.042",
            "1.0.0-dev.+42",
            "1.0.0-dev.18446744073709551616",
        ] {
            assert_eq!(dev_number(version), None, "{version}");
        }
    }

    #[test]
    fn a_child_without_markers_cannot_hide_a_marked_parent() {
        let end = Instant::now() + WAIT;
        let error = ancestry(8, end, |pid| match pid {
            8 => Ok(Some((7, false))),
            7 => Ok(Some((1, true))),
            _ => panic!("must stop at the agent parent"),
        })
        .unwrap_err();
        assert!(error.to_string().contains("agent-descended"));
        assert!(ancestry(8, end, |_| anyhow::bail!("unknown parent")).is_err());
        assert!(ancestry(8, end, |pid| Ok(Some((pid, false)))).is_err());
        assert!(ancestry(8, Instant::now(), |_| panic!("expired before inspection")).is_err());
    }

    // Isolate the post-authentication ordering boundary while using actual
    // kernel process currentness. Canonical/original-child auth is exercised
    // separately by the private CLI frontend-request integration fixture.
    #[cfg(unix)]
    fn live_authorized(request: Request, serial: u64) -> Authorized {
        Authorized {
            envelope: Envelope {
                abi: zerus_client_core::desktop::ui_requests::ABI.into(),
                attempt: id(101),
                request,
            },
            caller: Process::read(std::process::id()).unwrap(),
            frontend: Process::read(std::process::id()).unwrap(),
            until: Instant::now() + WAIT,
            instance: id(102),
            serial,
            ownership_origin: None,
            notification: None,
        }
    }

    #[test]
    #[cfg(unix)]
    fn activation_admission_uses_real_order_after_ownership_without_relaxing_freshness() {
        let mut order = Order::default();
        let grant = Request::DesktopOwnership {
            request_id: id(103),
            action: zerus_client_core::desktop::ownership::Action::Grant,
            journal: None,
            revision: 0,
        };
        live_authorized(grant, 5).into_request(&mut order).unwrap();
        live_authorized(
            Request::AutomaticApprovals {
                request_id: id(107),
                enabled: false,
            },
            7,
        )
        .into_request(&mut order)
        .unwrap();
        let activation = Request::ActivationWait {
            request_id: id(104),
        };
        let first = live_authorized(activation.clone(), 1)
            .into_request(&mut order)
            .unwrap();
        assert_eq!(first.activation_wait().unwrap().0, id(104));
        assert!(live_authorized(activation.clone(), 1)
            .into_request(&mut order)
            .is_err());
        assert!(live_authorized(activation.clone(), 0)
            .into_request(&mut order)
            .is_err());
        let mut expired = live_authorized(activation.clone(), 2);
        expired.until = Instant::now();
        assert!(expired.into_request(&mut order).is_err());
        let mut changed = live_authorized(activation.clone(), 2);
        changed.caller.identity.birth.push_str("-replaced");
        assert!(changed.into_request(&mut order).is_err());
        let mut changed = live_authorized(activation.clone(), 2);
        changed.frontend.identity.birth.push_str("-replaced");
        assert!(changed.into_request(&mut order).is_err());
        live_authorized(activation, 2)
            .into_request(&mut order)
            .unwrap()
            .activation_wait()
            .unwrap();
        let mut delayed = live_authorized(
            Request::ActivationWait {
                request_id: id(108),
            },
            3,
        )
        .into_request(&mut order)
        .unwrap();
        if let Freshness::Native { until, .. } = &mut delayed.1 {
            *until = Instant::now();
        }
        assert!(delayed.activation_wait().is_err());
    }

    #[test]
    fn unknown_frontend_operations_stay_closed_before_ordering() {
        let value = serde_json::json!({
            "abi": zerus_client_core::desktop::ui_requests::ABI,
            "attempt": id(105),
            "request": {"op": "unknown_operation", "request_id": id(106)}
        });
        assert!(serde_json::from_value::<Envelope>(value).is_err());
    }

    #[test]
    fn native_order_refuses_replay_and_slow_earlier_intents() {
        let mut order = Order::default();
        let now = Instant::now();
        let a = (7, "100".into(), id(1));
        let b = (7, "200".into(), id(2));
        order
            .observe(a.clone(), OrderedOperation::AutomaticApprovals, 2, now)
            .unwrap();
        assert!(order
            .observe(a.clone(), OrderedOperation::AutomaticApprovals, 1, now)
            .is_err());
        assert!(order
            .observe(a.clone(), OrderedOperation::AutomaticApprovals, 2, now)
            .is_err());
        order
            .observe(a.clone(), OrderedOperation::AutomaticApprovals, 3, now)
            .unwrap();
        order
            .observe(b.clone(), OrderedOperation::AutomaticApprovals, 1, now)
            .unwrap();
        assert!(order
            .observe(a.clone(), OrderedOperation::AutomaticApprovals, 4, now)
            .is_err());
        order
            .observe(b, OrderedOperation::AutomaticApprovals, 2, now)
            .unwrap();
        // An already authenticated request cannot survive WAIT, so the old
        // instance entry may expire without retaining unbounded history.
        order
            .observe(a, OrderedOperation::AutomaticApprovals, 5, now + WAIT)
            .unwrap();
    }

    #[test]
    fn cache_prepare_and_execute_share_their_own_typed_ordering_lane() {
        use zerus_client_core::desktop::cache_maintenance::Command;
        let commands = [
            Command::Prepare {
                operation_id: id(91),
                configuration: "a".repeat(64),
                root: "/srv/example/cache".into(),
                limit: 1,
                revision: 0,
                timing: zerus_client_core::desktop::cache_maintenance::Timing::Immediate,
                policy_change: true,
            },
            Command::Execute {
                configuration: "a".repeat(64),
                preparation_id: id(92),
                binding_sha256: "b".repeat(64),
            },
        ];
        let mut order = Order::default();
        let now = Instant::now();
        let instance = (7, "100".into(), id(1));
        for (serial, command) in (2..).zip(commands) {
            let request = Request::CacheMaintenance {
                request_id: id(serial),
                command: Box::new(command),
            };
            let lane = OrderedOperation::from(&request);
            assert_eq!(lane, OrderedOperation::CacheMaintenance);
            order
                .observe(instance.clone(), lane, serial as u64, now)
                .unwrap();
            assert!(order
                .observe(instance.clone(), lane, (serial - 1) as u64, now)
                .is_err());
            assert!(Validated::fixture(&serde_json::to_value(request).unwrap())
                .into_request()
                .is_err());
        }
        // A high cache serial neither rejects an unrelated activation read nor
        // lets its lower serial reset cache's original Prepare/Execute order.
        order
            .observe(instance.clone(), OrderedOperation::ActivationWait, 1, now)
            .unwrap();
        order
            .observe(instance.clone(), OrderedOperation::CreationClone, 1, now)
            .unwrap();
        assert!(order
            .observe(instance, OrderedOperation::CacheMaintenance, 2, now)
            .is_err());
    }

    #[test]
    fn ownership_actions_share_one_order_and_cannot_bypass_the_typed_boundary() {
        use zerus_client_core::desktop::ownership::Action;
        let mut order = Order::default();
        let now = Instant::now();
        let instance = (7, "100".into(), id(1));
        for (serial, action) in [
            (2, Action::Grant),
            (3, Action::NativeTakeover),
            (4, Action::Rearm),
        ] {
            let request = Request::DesktopOwnership {
                request_id: id(serial),
                action,
                journal: None,
                revision: 0,
            };
            order
                .observe(
                    instance.clone(),
                    OrderedOperation::from(&request),
                    serial as u64,
                    now,
                )
                .unwrap();
            assert!(order
                .observe(
                    instance.clone(),
                    OrderedOperation::from(&request),
                    (serial - 1) as u64,
                    now
                )
                .is_err());
            let value = serde_json::to_value(&request).unwrap();
            assert!(Validated::fixture(&value).into_request().is_err());
            assert!(
                Validated::fixture(&value).ownership_parts().is_err(),
                "no synthetic constructor grants native origin proof"
            );
        }
    }

    #[test]
    fn maintenance_does_not_obsolete_an_unrelated_approval_intent() {
        let mut order = Order::default();
        let now = Instant::now();
        let instance = (7, "100".into(), id(1));
        order
            .observe(
                instance.clone(),
                OrderedOperation::MaintenanceConfigure,
                2,
                now,
            )
            .unwrap();
        order
            .observe(
                instance.clone(),
                OrderedOperation::AutomaticApprovals,
                1,
                now,
            )
            .unwrap();
        order
            .observe(
                instance.clone(),
                OrderedOperation::AutomaticApprovals,
                3,
                now,
            )
            .unwrap();
        assert!(order
            .observe(instance, OrderedOperation::AutomaticApprovals, 1, now)
            .is_err());
    }

    #[test]
    fn agent_marker_names_are_exact_and_never_logged() {
        assert!(marked(b"PATH=/bin\0HGS_SESSION=example\0"));
        assert!(marked(b"HGS_RUN_ID=\0"));
        assert!(!marked(b"X_HGS_SESSION=example\0HGS_SESSION_EXTRA=x\0"));
        for key in [
            "LD_PRELOAD",
            "LD_AUDIT",
            "DYLD_INSERT_LIBRARIES",
            "DYLD_FORCE_FLAT_NAMESPACE",
        ] {
            assert!(marked(format!("{key}=\0").as_bytes()));
            assert!(!marked(format!("EXTRA_{key}=value\0").as_bytes()));
        }
    }

    #[test]
    fn trace_status_is_required_and_not_inferred_from_a_missing_field() {
        assert!(tracer_absent(b"Name:\texample\nTracerPid:\t0\n"));
        for bytes in [
            b"TracerPid: 123\n".as_slice(),
            b"Name: example\n",
            b"TracerPid: 0\nTracerPid: 0\n",
            b"TracerPid: unknown\n",
            b"TracerPid: 4294967296\n",
        ] {
            assert!(!tracer_absent(bytes));
        }
    }

    #[test]
    fn maintained_launcher_search_paths_are_not_injection_primitives() {
        let launcher=b"LD_LIBRARY_PATH=/opt/example/webkit/lib\0HGS_LINUX_WEBKIT_ORIGINAL_ENV={}\0DYLD_LIBRARY_PATH=/opt/example/lib\0DYLD_FRAMEWORK_PATH=/opt/example/Frameworks\0";
        assert!(!marked(launcher));
        let mut injected = launcher.to_vec();
        injected.extend_from_slice(b"LD_PRELOAD=/opt/example/inject.so\0");
        assert!(marked(&injected));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn frontend_custody_uses_the_explicit_installed_catalog_choice() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let prefix = temp.path().canonicalize().unwrap().join("selected");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(prefix.join("bin"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let image = prefix.join("bin/hgs-tray");
        std::fs::copy("/bin/cat", &image).unwrap();
        let other = prefix.join("bin/other");
        std::fs::copy("/bin/cat", &other).unwrap();
        let mut scope = Scope {
            location: Location {
                root: temp.path().join("state/desktop"),
                configuration: "a".repeat(64),
            },
            frontend_images: vec![image.clone()],
        };
        // Synthetic catalog and private files, not an installed GUI claim.
        let selected = scope.select_frontend(&image).unwrap();
        assert_eq!(selected.image(), image);
        assert_eq!(selected.configuration(), scope.configuration);
        selected.revalidate().unwrap();
        assert!(scope.select_frontend(&other).is_err());
        // HOME's maintained prefix may itself be a symlink. Its catalog entry
        // still identifies the same file; custody pins the canonical layout.
        let linked = temp.path().join(".local");
        std::os::unix::fs::symlink(&prefix, &linked).unwrap();
        scope.frontend_images.insert(0, linked.join("bin/hgs-tray"));
        let selected = scope.select_frontend(&image).unwrap();
        assert_eq!(selected.image(), image);
        selected.revalidate().unwrap();
    }

    #[test]
    fn a_native_looking_filename_does_not_select_the_installed_frontend() {
        let temp = tempfile::tempdir().unwrap();
        let installed = temp.path().join("installed/hgs-tray");
        let other = temp.path().join("other/hgs-tray");
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&installed, b"installed fixture").unwrap();
        std::fs::write(&other, b"same name, another program").unwrap();
        let scope = Scope {
            location: Location {
                root: temp.path().join("state/desktop"),
                configuration: "a".repeat(64),
            },
            frontend_images: vec![installed.clone()],
        };
        assert!(scope.selected_frontend(&installed));
        assert!(!scope.selected_frontend(&other));
        assert!(!scope.selected_frontend(&temp.path().join("missing/hgs-tray")));
    }
}

#[cfg(test)]
mod activation_freshness_tests {
    use super::*;
    fn request() -> Request {
        Request::ActivationWait {
            request_id: uuid::Uuid::new_v4().to_string(),
        }
    }
    #[test]
    fn an_expired_receiver_stays_expired() {
        let receiver = ActivationReceiver::new(Freshness::Fixture, Instant::now() - WAIT);
        assert!(!receiver.current());
    }
    #[test]
    fn busy_turns_keep_live_checks_but_full_proof_waits_for_its_deadline() {
        use std::sync::{atomic::Ordering, Arc};
        let mut receiver = ActivationReceiver::new(
            Freshness::Fixture,
            Instant::now() + Duration::from_secs(3600),
        );
        let probe = Arc::new(ActivationProbe::default());
        receiver.set_probe(probe.clone(), Instant::now() + Duration::from_secs(3600));
        // Control the cached-check deadline rather than rely on wall time.
        receiver
            .next_full_check
            .set(Instant::now() + Duration::from_secs(3600));
        for _ in 0..10_000 {
            assert!(receiver.current());
        }
        assert_eq!(probe.live_checks.load(Ordering::SeqCst), 10_000);
        assert_eq!(probe.full_checks.load(Ordering::SeqCst), 0);
        receiver.next_full_check.set(Instant::now() - WAIT);
        assert!(receiver.current());
        assert_eq!(probe.full_checks.load(Ordering::SeqCst), 1);
        probe.live.store(false, Ordering::SeqCst);
        assert!(!receiver.current());
        assert_eq!(probe.full_checks.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn every_handoff_bypasses_the_cached_proof_and_detects_revocation() {
        use std::sync::{atomic::Ordering, Arc};
        let mut receiver = ActivationReceiver::new(
            Freshness::Fixture,
            Instant::now() + Duration::from_secs(3600),
        );
        let probe = Arc::new(ActivationProbe::default());
        receiver.set_probe(probe.clone(), Instant::now() + Duration::from_secs(3600));
        assert!(receiver.handoff_current());
        assert!(receiver.handoff_current());
        assert_eq!(probe.full_checks.load(Ordering::SeqCst), 2);
        receiver
            .next_full_check
            .set(Instant::now() + Duration::from_secs(3600));
        probe.valid.store(false, Ordering::SeqCst);
        assert!(receiver.current(), "periodic cache is still recent");
        assert!(!receiver.handoff_current());
        assert_eq!(probe.full_checks.load(Ordering::SeqCst), 3);
    }
    #[cfg(not(windows))]
    #[test]
    fn receiver_admission_does_not_extend_the_original_mutation_expiry() {
        let caller = Process::read(std::process::id()).unwrap();
        let frontend = Process::read(std::process::id()).unwrap();
        assert!(caller.current() && frontend.current());
        let freshness = Freshness::Native {
            caller,
            frontend,
            until: Instant::now() - WAIT,
        };
        assert!(Validated(request(), freshness, Metadata::default())
            .activation_wait()
            .is_err());
    }
    #[cfg(windows)]
    #[test]
    fn live_processes_without_original_windows_evidence_cannot_receive_clicks() {
        let live = || {
            let caller = Process::read(std::process::id()).unwrap();
            let frontend = Process::read(std::process::id()).unwrap();
            assert!(caller.current() && frontend.current());
            Freshness::Native {
                caller,
                frontend,
                until: Instant::now() + WAIT,
                windows: None,
            }
        };
        assert!(Validated(request(), live(), Metadata::default())
            .activation_wait()
            .is_err());
        let receiver = ActivationReceiver::new(
            live(),
            Instant::now() + zerus_client_core::desktop::activation::PEER_WAIT,
        );
        assert!(!receiver.current());
        assert!(!receiver.handoff_current());
    }
}
mod notification;
pub(super) use notification::FocusPeer;
