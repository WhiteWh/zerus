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
#[cfg(target_os = "linux")]
mod linux_image;
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

// New closed own-helper entry only. Not selected-CLI equivalence, a service
// reference, or an independent native permission; caller wiring is separate.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
mod program_helper_image;

pub(super) struct Scope {
    location: Location,
    frontend_images: Vec<PathBuf>,
    /// Windows: the portable zip's app as its own staging bound it to this
    /// process's CLI; each use proves the app again.
    #[cfg_attr(not(windows), allow(dead_code))]
    portable: Option<super::portable_frontend::PortableScope>,
    #[cfg(target_os = "linux")]
    sealed_reference: Option<std::sync::Arc<linux_image::Reference>>,
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
            portable: super::portable_frontend::materialize(),
            #[cfg(target_os = "linux")]
            sealed_reference: None,
        }
    }
    /// The service calls this once under its actual owner before listening.
    /// A failed/late snapshot is never replaced from an installed pathname.
    #[cfg(target_os = "linux")]
    pub(super) fn initialize_reference(&mut self, owner: &std::sync::Arc<platform::FileLock>) {
        self.sealed_reference = linux_image::Reference::capture(owner);
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
    /// A running GUI that is the portable zip's bound app: its proof, bound to
    /// that process and its loaded image, within `until`.
    #[cfg(windows)]
    pub(super) fn portable_running(
        &self,
        original: &Original,
        until: Instant,
    ) -> Option<std::sync::Arc<super::portable_frontend::GuiProof>> {
        self.portable.as_ref()?.running(original, until)
    }
    /// With the UI closed: the bound portable app pinned and proven for a launch.
    #[cfg(windows)]
    pub(super) fn portable_closed(
        &self,
        image: &Path,
        until: Instant,
    ) -> Option<zerus_client_core::desktop::windows::image::PinnedImage> {
        self.portable.as_ref()?.closed(image, until)
    }
    /// Bind an explicit maintained-image choice; this never discovers one from
    /// PATH or turns an inbound path check into a launch/worker permission.
    #[cfg(any(target_os = "linux", windows))]
    pub fn select_frontend(
        &self,
        image: &Path,
    ) -> Result<super::notifications::frontend::SelectedFrontend> {
        super::notifications::frontend::SelectedFrontend::select(self, image)
    }
}

// Distinct private freshness cases. Own-helper observations cannot construct
// a service-reference Peer or claim equivalence to the startup service image.
#[cfg(target_os = "linux")]
enum LinuxFreshness {
    Service(std::sync::Arc<linux_image::Peer>),
    Own {
        image: program_helper_image::OwnImage,
        until: Instant,
    },
}
#[derive(Clone)]
struct Process {
    pid: u32,
    identity: platform::ProcessIdentity,
    executable: String,
    #[cfg(target_os = "linux")]
    linux_freshness: Option<std::sync::Arc<LinuxFreshness>>,
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
            #[cfg(target_os = "linux")]
            linux_freshness: None,
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
        #[cfg(target_os = "linux")]
        if let Some(LinuxFreshness::Own {image, until}) = self.linux_freshness.as_deref() {
            return self.pid == std::process::id()
                && platform::process_identity(self.pid).as_ref() == Some(&self.identity)
                && image.check(*until).is_ok()
                && platform::process_identity(self.pid).as_ref() == Some(&self.identity)
                && Instant::now() < *until;
        }
        #[cfg(target_os = "linux")]
        if let Some(LinuxFreshness::Service(image)) = self.linux_freshness.as_deref() {
            // Exec retains PID/birth and can retain the SAME memfd display name.
            // Reopen the kernel object at EVERY original freshness boundary.
            return platform::process_identity(self.pid).as_ref() == Some(&self.identity)
                && image.current(self.pid)
                && platform::process_identity(self.pid).as_ref() == Some(&self.identity);
        }
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
    /// The portable app's proof for this exact frontend, held to the end.
    portable: Option<std::sync::Arc<super::portable_frontend::GuiProof>>,
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
            self.portable.as_ref().is_none_or(|proof| proof.current()),
            "portable frontend proof expired"
        );
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
    /// The verified frontend's own application identity (its hello).
    application: String,
    #[cfg(windows)]
    windows: Option<Box<WindowsFreshness>>,
}
/// Actor-side permit after the original peer was revalidated. It deliberately
/// has no Clone/Deserialize or public constructor, even inside the request lane.
#[derive(Default)]
struct Metadata {
    ownership: Option<super::ownership::Origin>,
    notification: Option<zerus_client_core::desktop::notification_control::NativeControl>,
    application: String,
}
pub(super) struct Validated(Request, Freshness, Metadata);

/// One new closed helper's initial proof, not a generic executing actor kind.
/// The worker must reserve the SAME original UUID durably before consuming it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code)]
pub(crate) struct ProgramStopAdmission(Validated);
#[cfg(any(target_os = "linux", target_os = "macos"))]
enum ProgramLife {
    Native(Box<ProgramOriginal>),
    #[cfg(test)]
    Fixture,
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct ProgramOriginal {
    caller: Process,
    frontend: Process,
    source: super::ownership::programs::Current,
}
/// The original one-shot permission after proof consumption. It cannot be
/// reconstructed from a report or renewed by another cache/serial/child.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code)]
pub(crate) struct ProgramStopPermit<'original> {
    intent: zerus_client_core::worktree_programs::invocation::Intent,
    life: ProgramLife,
    until: Instant,
    reservation: &'original crate::state::ProgramStopClaim,
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code)]
impl ProgramStopAdmission {
    pub(crate) fn consume<'original>(
        self,
        expected: &zerus_client_core::worktree_programs::invocation::Intent,
        until: Instant,
        location: &Location,
        reservation: &'original crate::state::ProgramStopClaim,
    ) -> Result<ProgramStopPermit<'original>> {
        reservation.current().map_err(anyhow::Error::msg)?;
        ensure!(
            reservation.input() == expected.input.as_ref(),
            "original program-stop reservation changed"
        );
        self.0.check()?;
        ensure!(
            until > Instant::now()
                && until.saturating_duration_since(Instant::now())
                    <= Duration::from_millis(
                        zerus_client_core::worktree_programs::custody::PHASE_MILLIS
                    ),
            "program-stop operation expired"
        );
        let Validated(request, freshness, metadata) = self.0;
        let Request::WorktreeProgramStop { intent } = request else {
            anyhow::bail!("expected program-stop proof")
        };
        ensure!(
            intent.as_ref() == expected,
            "original program-stop input changed"
        );
        let life = match freshness {
            Freshness::Native {
                caller,
                frontend,
                until: proof_until,
            } => {
                ensure!(
                    location.configuration == intent.input.manifest.configuration,
                    "program-stop configuration changed"
                );
                let origin = metadata
                    .ownership
                    .context("original program-stop frontend origin missing")?;
                let source = super::ownership::programs::Current::capture(
                    location,
                    &intent.input.source,
                    origin,
                    proof_until,
                )?;
                ensure!(
                    Instant::now() < proof_until
                        && caller.pid == std::process::id()
                        && caller.current()
                        && frontend.current()
                        && Instant::now() < proof_until,
                    "original program-stop proof expired"
                );
                #[cfg(target_os = "linux")]
                ensure!(
                    matches!(
                        caller.linux_freshness.as_deref(),
                        Some(LinuxFreshness::Own { .. })
                    ),
                    "own-helper freshness missing"
                );
                ProgramLife::Native(Box::new(ProgramOriginal {
                    caller,
                    frontend,
                    source,
                }))
            }
            #[cfg(test)]
            Freshness::Fixture => ProgramLife::Fixture,
        };
        // Original proof is consumed here. This operation bound was supplied
        // by the already-running original job, not a replacement proof TTL.
        reservation.spend().map_err(anyhow::Error::msg)?;
        Ok(ProgramStopPermit {
            intent: *intent,
            life,
            until,
            reservation,
        })
    }
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code)]
impl ProgramStopPermit<'_> {
    pub(crate) fn intent(&self) -> &zerus_client_core::worktree_programs::invocation::Intent {
        &self.intent
    }
    /// Current original native objects only, not another authentication or
    /// consent. Root independently checks current ownership/Grant, checkout,
    /// original process pin/membership and cancellation before EACH signal.
    pub(crate) fn current(&self) -> Result<()> {
        self.reservation.current().map_err(anyhow::Error::msg)?;
        ensure!(
            Instant::now() < self.until,
            "original program-stop operation expired"
        );
        match &self.life {
            ProgramLife::Native(original) => {
                let ProgramOriginal {
                    caller,
                    frontend,
                    source,
                } = original.as_ref();
                source.check(self.until)?;
                ensure!(
                    caller.pid == std::process::id() && frontend.current(),
                    "original program-stop source changed"
                );
                #[cfg(target_os = "linux")]
                {
                    let Some(LinuxFreshness::Own {
                        image,
                        until: _original_proof_until,
                    }) = caller.linux_freshness.as_deref()
                    else {
                        anyhow::bail!("own helper missing")
                    };
                    ensure!(
                        platform::process_identity(caller.pid).as_ref() == Some(&caller.identity),
                        "original helper changed"
                    );
                    image.check(self.until)?;
                    ensure!(
                        platform::process_identity(caller.pid).as_ref() == Some(&caller.identity),
                        "original helper changed"
                    );
                }
                #[cfg(target_os = "macos")]
                ensure!(caller.current(), "original helper changed");
                check_ancestry(caller.pid, self.until)?;
                ensure!(frontend.current(), "original native frontend changed");
            }
            #[cfg(test)]
            ProgramLife::Fixture => (),
        }
        ensure!(
            Instant::now() < self.until,
            "original program-stop operation expired"
        );
        Ok(())
    }
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code)] // Only the new private final-cleanup command consumes this.
pub(crate) struct ProgramCleanupAdmission(Validated);
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code)]
pub(crate) struct ProgramCleanupPermit<'original> {
    intent: zerus_client_core::worktree_programs::cleanup::Intent,
    life: ProgramLife,
    until: Instant,
    reservation: &'original crate::state::ProgramRemovalClaim,
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code)]
impl ProgramCleanupAdmission {
    pub(crate) fn consume<'original>(
        self,
        expected: &zerus_client_core::worktree_programs::cleanup::Intent,
        until: Instant,
        location: &Location,
        reservation: &'original crate::state::ProgramRemovalClaim,
    ) -> Result<ProgramCleanupPermit<'original>> {
        reservation.current().map_err(anyhow::Error::msg)?;
        ensure!(
            reservation.input() == expected.input.as_ref(),
            "original program-cleanup reservation changed"
        );
        self.0.check()?;
        ensure!(
            until > Instant::now()
                && until.saturating_duration_since(Instant::now())
                    <= Duration::from_millis(
                        zerus_client_core::worktree_programs::custody::PHASE_MILLIS
                    ),
            "program-cleanup operation expired"
        );
        let Validated(request, freshness, metadata) = self.0;
        let Request::WorktreeProgramCleanup { intent } = request else {
            anyhow::bail!("expected program-cleanup proof")
        };
        ensure!(
            intent.as_ref() == expected,
            "original program-cleanup input changed"
        );
        let life = match freshness {
            Freshness::Native {
                caller,
                frontend,
                until: proof_until,
            } => {
                ensure!(
                    location.configuration == intent.input.stop.manifest.configuration,
                    "program-cleanup configuration changed"
                );
                let origin = metadata
                    .ownership
                    .context("original program-cleanup frontend origin missing")?;
                let source = super::ownership::programs::Current::capture(
                    location,
                    &intent.input.stop.source,
                    origin,
                    proof_until,
                )?;
                ensure!(
                    Instant::now() < proof_until
                        && caller.pid == std::process::id()
                        && caller.current()
                        && frontend.current()
                        && Instant::now() < proof_until,
                    "original program-cleanup proof expired"
                );
                #[cfg(target_os = "linux")]
                ensure!(
                    matches!(
                        caller.linux_freshness.as_deref(),
                        Some(LinuxFreshness::Own { .. })
                    ),
                    "own-helper freshness missing"
                );
                ProgramLife::Native(Box::new(ProgramOriginal {
                    caller,
                    frontend,
                    source,
                }))
            }
            #[cfg(test)]
            Freshness::Fixture => ProgramLife::Fixture,
        };
        // Original proof is consumed here. This operation bound was supplied
        // by the already-running original job, not a replacement proof TTL.
        reservation.spend().map_err(anyhow::Error::msg)?;
        Ok(ProgramCleanupPermit {
            intent: *intent,
            life,
            until,
            reservation,
        })
    }
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code)]
impl ProgramCleanupPermit<'_> {
    pub(crate) fn intent(&self) -> &zerus_client_core::worktree_programs::cleanup::Intent {
        &self.intent
    }
    /// Current original native objects only, not another authentication or
    /// consent. Root independently checks current ownership/Grant, checkout,
    /// original process pin/membership and cancellation before EACH signal.
    pub(crate) fn current(&self) -> Result<()> {
        self.reservation.current().map_err(anyhow::Error::msg)?;
        ensure!(
            Instant::now() < self.until,
            "original program-cleanup operation expired"
        );
        match &self.life {
            ProgramLife::Native(original) => {
                let ProgramOriginal {
                    caller,
                    frontend,
                    source,
                } = original.as_ref();
                source.check(self.until)?;
                ensure!(
                    caller.pid == std::process::id() && frontend.current(),
                    "original program-cleanup source changed"
                );
                #[cfg(target_os = "linux")]
                {
                    let Some(LinuxFreshness::Own {
                        image,
                        until: _original_proof_until,
                    }) = caller.linux_freshness.as_deref()
                    else {
                        anyhow::bail!("own helper missing")
                    };
                    ensure!(
                        platform::process_identity(caller.pid).as_ref() == Some(&caller.identity),
                        "original helper changed"
                    );
                    image.check(self.until)?;
                    ensure!(
                        platform::process_identity(caller.pid).as_ref() == Some(&caller.identity),
                        "original helper changed"
                    );
                }
                #[cfg(target_os = "macos")]
                ensure!(caller.current(), "original helper changed");
                check_ancestry(caller.pid, self.until)?;
                ensure!(frontend.current(), "original native frontend changed");
            }
            #[cfg(test)]
            ProgramLife::Fixture => (),
        }
        ensure!(
            Instant::now() < self.until,
            "original program-cleanup operation expired"
        );
        Ok(())
    }
}

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
    /// The verified frontend's application identity from its hello: which
    /// build asked, never a permission by itself.
    pub fn application(&self) -> &str {
        &self.2.application
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
                    | Request::DesktopOwnershipObserve { .. }
                    | Request::NotificationControl { .. }
                    | Request::CacheMaintenance { .. }
                    | Request::CreationClone { .. }
                    | Request::CreationWorktree { .. }
                    | Request::CreationLaunch { .. }
                    | Request::TaskMessage { .. }
                    | Request::TaskMessageInputRelease { .. }
                    | Request::DeferredSend { .. }
                    | Request::DeferredAnswer { .. }
                    | Request::CompactToSend { .. }
                    | Request::SessionResume { .. }
                    | Request::TaskStartReconcile { .. }
                    | Request::TerminationOutcome { .. }
                    | Request::WorktreeProgramStop { .. }
            | Request::WorktreeProgramCleanup { .. }
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
    /// Only the launch actor calls this after BEGIN IMMEDIATE. Cached native
    /// Service mode was bound to this exact original request during admission.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn creation_launch(self) -> Result<zerus_client_core::desktop::creation_launch::Intent> {
        self.check()?;
        let Request::CreationLaunch { intent } = self.0 else {
            anyhow::bail!("not a creation launch");
        };
        Ok(*intent)
    }
    /// Only the deferred send actor calls this, after BEGIN IMMEDIATE.
    /// Cached native Service mode was bound to this exact original request
    /// during admission.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn deferred_send(self) -> Result<zerus_client_core::desktop::deferred_send::Intent> {
        self.check()?;
        let Request::DeferredSend { intent } = self.0 else {
            anyhow::bail!("not a deferred send");
        };
        Ok(*intent)
    }
    /// The connection a deferred-input request (send, answer, compact-to-send
    /// or session resume) was made for, read
    /// before its admission. It grants and consumes nothing: the actor still
    /// consumes this permit after BEGIN IMMEDIATE. A refusal before that
    /// transaction reads it only to tell this epoch's own request from
    /// another connection's, which it never answers not admitted
    /// (zerus-tlb9q).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn deferred_input_scope(
        &self,
    ) -> Option<&zerus_client_core::desktop::creation_clone::Scope> {
        match &self.0 {
            Request::DeferredSend { intent } => Some(&intent.scope),
            Request::DeferredAnswer { intent } => Some(&intent.scope),
            Request::CompactToSend { intent } => Some(&intent.scope),
            Request::SessionResume { intent } => Some(&intent.scope),
            _ => None,
        }
    }
    /// Only the deferred answer actor calls this, after BEGIN IMMEDIATE.
    /// Cached native Service mode was bound to this exact original request
    /// during admission.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn deferred_answer(self) -> Result<zerus_client_core::desktop::deferred_answer::Intent> {
        self.check()?;
        let Request::DeferredAnswer { intent } = self.0 else {
            anyhow::bail!("not a deferred answer");
        };
        Ok(*intent)
    }
    /// Only the compact-to-send actor calls this, after BEGIN IMMEDIATE.
    /// Cached native Service mode was bound to this exact original request
    /// during admission.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn compact_to_send(self) -> Result<zerus_client_core::desktop::compact_to_send::Intent> {
        self.check()?;
        let Request::CompactToSend { intent } = self.0 else {
            anyhow::bail!("not a compact-to-send");
        };
        Ok(*intent)
    }
    /// Only the session resume actor calls this, after BEGIN IMMEDIATE.
    /// Cached native Service mode was bound to this exact original request
    /// during admission.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn session_resume(self) -> Result<zerus_client_core::desktop::session_resume::Intent> {
        self.check()?;
        let Request::SessionResume { intent } = self.0 else {
            anyhow::bail!("not a session resume");
        };
        Ok(*intent)
    }
    /// Only the task message actor calls this after BEGIN IMMEDIATE. Cached
    /// native Service mode was bound to this exact original request during
    /// admission.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn task_message(self) -> Result<zerus_client_core::desktop::task_message::Intent> {
        self.check()?;
        let Request::TaskMessage { intent } = self.0 else {
            anyhow::bail!("not a task message");
        };
        Ok(*intent)
    }
    /// A read of the original before the actor's preparation. It grants
    /// nothing: admission still consumes this permit through `task_message`
    /// after BEGIN IMMEDIATE, and an expired permit refuses there.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn task_message_preview(
        &self,
    ) -> Result<&zerus_client_core::desktop::task_message::Intent> {
        self.check()?;
        let Request::TaskMessage { intent } = &self.0 else {
            anyhow::bail!("not a task message");
        };
        Ok(intent)
    }
    /// Only the task message runtime calls this, after BEGIN IMMEDIATE, for
    /// one exact claim of a reviewed custody input.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn task_message_input_release(
        self,
    ) -> Result<zerus_client_core::desktop::task_message::review::Release> {
        self.check()?;
        let Request::TaskMessageInputRelease { release } = self.0 else {
            anyhow::bail!("not a task message input release");
        };
        Ok(*release)
    }
    /// A read of the reviewed release before its claim. It grants nothing:
    /// the claim still consumes this permit after BEGIN IMMEDIATE.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn task_message_input_release_preview(
        &self,
    ) -> Result<&zerus_client_core::desktop::task_message::review::Release> {
        self.check()?;
        let Request::TaskMessageInputRelease { release } = &self.0 else {
            anyhow::bail!("not a task message input release");
        };
        Ok(release)
    }
    /// Only the worktree actor calls this after BEGIN IMMEDIATE. Cached native
    /// Service mode was bound to this exact original request during admission.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn creation_worktree(
        self,
    ) -> Result<zerus_client_core::desktop::creation_worktree::Intent> {
        self.check()?;
        let Request::CreationWorktree { intent } = self.0 else {
            anyhow::bail!("not a creation worktree");
        };
        Ok(*intent)
    }
    /// Only the per-kind actor may consume this after BEGIN IMMEDIATE, then
    /// recheck its actual current source/lease and freeze the original bytes.
    #[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
    pub fn task_start_reconcile(
        self,
    ) -> Result<zerus_client_core::desktop::task_start_reconcile::Intent> {
        self.check()?;
        let Request::TaskStartReconcile { intent } = self.0 else {
            anyhow::bail!("not a task Start reconciliation");
        };
        Ok(*intent)
    }
    /// Data for bounded native preparation, without consuming or extending the
    /// original proof. The actor still consumes it after BEGIN IMMEDIATE.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn task_start_preview(
        &self,
    ) -> Result<&zerus_client_core::desktop::task_start_reconcile::Intent> {
        self.check()?;
        let Request::TaskStartReconcile { intent } = &self.0 else {
            anyhow::bail!("not task Start")
        };
        Ok(intent)
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
    /// A native requester observation is deliberately separate from mutation
    /// admission. This accessor cannot be consumed by the ownership writer.
    pub fn ownership_observation(
        &self,
    ) -> Result<(zerus_client_core::desktop::ownership::observation::Query, super::ownership::Origin)> {
        self.check()?;
        let Request::DesktopOwnershipObserve { query } = &self.0 else {
            anyhow::bail!("not an ownership observation");
        };
        let origin = self.2.ownership.as_ref().context("original ownership identity missing")?;
        ensure!(origin.current(), "ownership image changed");
        Ok((query.as_ref().clone(), origin.clone()))
    }
    /// Only original-outcome retention consumes this, after BEGIN IMMEDIATE.
    pub fn termination_outcome_parts(
        &self,
    ) -> Result<&zerus_client_core::desktop::termination_outcome::Intent> {
        self.check()?;
        let Request::TerminationOutcome { intent } = &self.0 else {
            anyhow::bail!("not termination outcome retention");
        };
        ensure!(intent.valid(), "invalid original termination capture/plan");
        Ok(intent)
    }
    pub fn check_termination_outcome(self) -> Result<()> {
        self.termination_outcome_parts().map(|_| ())
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
    /// Actor policy model only: production observations obtain this Origin
    /// from authenticate(), never from a supplied value.
    #[cfg(test)]
    pub fn fixture_observation(value: &Value, origin: super::ownership::Origin) -> Self {
        let mut permit = Self::fixture(value);
        permit.2.ownership = Some(origin);
        permit
    }
    /// A fixture from a frontend whose hello names `application`.
    #[cfg(test)]
    pub fn fixture_from(value: &Value, application: &str) -> Self {
        let mut permit = Self::fixture(value);
        permit.2.application = application.into();
        permit
    }
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub fn fixture_expiring(value: &Value, until: Instant) -> Self {
        let mut permit = Self::fixture(value);
        permit.1 = Freshness::Native {
            caller: Process::read(std::process::id()).unwrap(),
            frontend: Process::read(std::process::id()).unwrap(),
            until,
        };
        permit
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
    /// Windows: a toast activation gives this service a short-lived foreground
    /// right. Lend it to the exact authenticated frontend just before its click
    /// reply. The held process handle keeps the PID bound to that process. A
    /// refusal only means no right is held now; the frontend keeps its fallback.
    #[cfg(any(windows, test))]
    pub fn lend_foreground(&self) -> bool {
        #[cfg(test)]
        if let Some(probe) = &self.probe {
            probe
                .foreground
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return probe
                .foreground_right
                .load(std::sync::atomic::Ordering::SeqCst);
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
            match &self.freshness {
                // SAFETY: a plain Win32 call; the original handle is still held.
                Freshness::Native { frontend, .. } => {
                    frontend.current() && unsafe { AllowSetForegroundWindow(frontend.pid) } != 0
                }
                #[cfg(test)]
                Freshness::Fixture => false,
            }
        }
        #[cfg(not(windows))]
        false
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
    pub foreground: std::sync::atomic::AtomicUsize,
    pub foreground_right: std::sync::atomic::AtomicBool,
}
#[cfg(test)]
impl Default for ActivationProbe {
    fn default() -> Self {
        Self {
            live: std::sync::atomic::AtomicBool::new(true),
            valid: std::sync::atomic::AtomicBool::new(true),
            live_checks: std::sync::atomic::AtomicUsize::new(0),
            full_checks: std::sync::atomic::AtomicUsize::new(0),
            foreground: std::sync::atomic::AtomicUsize::new(0),
            foreground_right: std::sync::atomic::AtomicBool::new(true),
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[allow(dead_code)]
    pub(super) fn into_program_stop(self, order: &mut Order) -> Result<ProgramStopAdmission> {
        ensure!(
            matches!(self.envelope.request, Request::WorktreeProgramStop { .. }),
            "expected closed program-stop proof"
        );
        Ok(ProgramStopAdmission(self.into_request(order)?))
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[allow(dead_code)]
    pub(super) fn into_program_cleanup(self, order: &mut Order) -> Result<ProgramCleanupAdmission> {
        ensure!(matches!(self.envelope.request, Request::WorktreeProgramCleanup { .. }), "expected closed program-cleanup proof");
        Ok(ProgramCleanupAdmission(self.into_request(order)?))
    }
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
                application: self.application,
            },
        ))
    }
}

// The synchronization prefix does not broaden the permitted relay command.
// Mac alone uses it; duplicate/misplaced markers and extra arguments refuse.
fn frontend_request_arguments(args: &[String], rendezvous: bool) -> bool {
    #[cfg(unix)]
    let args = if rendezvous
        && args.first().is_some_and(|arg| arg == zerus_client_core::run::rendezvous::ARG)
    {
        &args[1..]
    } else {
        args
    };
    #[cfg(not(unix))]
    let _ = rendezvous;
    args == ["desktop", "frontend-request", "--json"]
}

pub(super) fn authenticate(location: &Scope, peer_pid: u32, value: &Value) -> Result<Authorized> {
    let envelope: Envelope =
        serde_json::from_value(value.clone()).context("invalid frontend envelope")?;
    ensure!(envelope.valid(), "invalid frontend envelope");
    ensure!(!matches!(envelope.request,Request::WorktreeProgramStop {..} | Request::WorktreeProgramCleanup {..}),
        "program-stop requires its closed original helper entry");
    let until = Instant::now() + WAIT;
    let caller = Process::read(peer_pid)?;
    #[cfg(target_os = "linux")]
    let caller = {
        let mut caller = caller;
        let selected = platform::describe(std::process::id())
            .and_then(|own| platform::file_identity(Path::new(&own.executable)));
        if !selected
            .is_some_and(|id| platform::file_identity(Path::new(&caller.executable)) == Some(id))
        {
            // Only this old image predicate gains an immutable equivalence
            // branch. All native child/argv/config/ancestry checks remain below.
            caller.linux_freshness = Some(std::sync::Arc::new(LinuxFreshness::Service(
                location
                    .sealed_reference
                    .as_ref()
                    .context("sealed service relay unavailable")?
                    .observe(&caller, until)?,
            )));
            ensure!(caller.current(), "sealed frontend relay changed");
        }
        caller
    };
    #[cfg(not(target_os = "linux"))]
    {
        let own_image =
            platform::describe(std::process::id()).context("service image unavailable")?;
        let selected = platform::file_identity(Path::new(&own_image.executable))
            .context("service image identity unavailable")?;
        ensure!(
            platform::file_identity(Path::new(&caller.executable)) == Some(selected),
            "frontend relay is not the selected hgs"
        );
    }
    let argv = caller.arguments()?;
    ensure!(
        argv.get(1..).is_some_and(|args| {
            frontend_request_arguments(args, cfg!(target_os = "macos"))
        }),
        "unexpected frontend relay"
    );
    authenticate_caller(location,peer_pid,envelope,caller,until)
}

/// Fixed own-helper entry only. The self object supplies freshness, not
/// selected-CLI authentication: the original native parent's image gate and
/// registration plus the common canonical probe/validate remain mandatory.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code)] // Wired only by the new closed stdio owner, never the generic actor.
pub(super) fn authenticate_program_stop(location: &Scope, value: &Value) -> Result<Authorized> {
    let envelope: Envelope =
        serde_json::from_value(value.clone()).context("invalid program-stop envelope")?;
    ensure!(
        envelope.valid() && matches!(envelope.request, Request::WorktreeProgramStop { .. }),
        "expected closed program-stop request"
    );
    let until = Instant::now() + WAIT;
    let pid = std::process::id();
    #[allow(unused_mut)]
    let mut caller = Process::read(pid)?;
    #[cfg(target_os = "linux")]
    {
        caller.linux_freshness = Some(std::sync::Arc::new(LinuxFreshness::Own {
            image:program_helper_image::OwnImage::capture(until)?,
            until,
        }));
    }
    #[cfg(target_os = "macos")]
    {
        let own = platform::describe(pid).context("original helper image unavailable")?;
        let selected = platform::file_identity(Path::new(&own.executable))
            .context("original helper image unavailable")?;
        ensure!(
            platform::file_identity(Path::new(&caller.executable)) == Some(selected),
            "original helper image changed"
        );
    }
    let argv = caller.arguments()?;
    let args = argv.get(1..).context("original helper arguments missing")?;
    #[cfg(target_os = "macos")]
    let args = if args
        .first()
        .is_some_and(|arg| arg == zerus_client_core::run::rendezvous::ARG)
    {
        &args[1..]
    } else {
        args
    };
    ensure!(
        args == ["worktrees", "program-stop", "--json"],
        "unexpected program-stop helper command"
    );
    ensure!(
        caller.current() && Instant::now() < until,
        "original helper expired"
    );
    authenticate_caller(location, pid, envelope, caller, until)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code)]
pub(super) fn authenticate_program_cleanup(location: &Scope, value: &Value) -> Result<Authorized> {
    let envelope: Envelope =
        serde_json::from_value(value.clone()).context("invalid program-cleanup envelope")?;
    ensure!(
        envelope.valid() && matches!(envelope.request, Request::WorktreeProgramCleanup { .. }),
        "expected closed program-cleanup request"
    );
    let until = Instant::now() + WAIT;
    let pid = std::process::id();
    #[allow(unused_mut)]
    let mut caller = Process::read(pid)?;
    #[cfg(target_os = "linux")]
    {
        caller.linux_freshness = Some(std::sync::Arc::new(LinuxFreshness::Own {
            image:program_helper_image::OwnImage::capture_cleanup(until)?,
            until,
        }));
    }
    #[cfg(target_os = "macos")]
    {
        let own = platform::describe(pid).context("original helper image unavailable")?;
        let selected = platform::file_identity(Path::new(&own.executable))
            .context("original helper image unavailable")?;
        ensure!(
            platform::file_identity(Path::new(&caller.executable)) == Some(selected),
            "original helper image changed"
        );
    }
    let argv = caller.arguments()?;
    let args = argv.get(1..).context("original helper arguments missing")?;
    #[cfg(target_os = "macos")]
    let args = if args
        .first()
        .is_some_and(|arg| arg == zerus_client_core::run::rendezvous::ARG)
    {
        &args[1..]
    } else {
        args
    };
    ensure!(
        args == ["worktrees", "program-cleanup", "--json"],
        "unexpected program-cleanup helper command"
    );
    ensure!(
        caller.current() && Instant::now() < until,
        "original helper expired"
    );
    authenticate_caller(location, pid, envelope, caller, until)
}

fn authenticate_caller(
    location: &Scope,
    peer_pid: u32,
    envelope: Envelope,
    caller: Process,
    until: Instant,
) -> Result<Authorized> {
    let Canonical {
        pid: frontend_pid,
        reply,
        #[cfg(windows)]
        peer,
    } = probe(location, &envelope.attempt, until)
        .context("canonical frontend evidence unavailable")?;
    let frontend = Process::read(frontend_pid)?;
    // The portable zip's bound app is proven for this exact process within the
    // same original deadline; the installer and adjacent images are unchanged.
    #[cfg(windows)]
    let portable = if location.selected_frontend(Path::new(&frontend.executable)) {
        None
    } else {
        Some(
            frontend
                .original
                .as_deref()
                .and_then(|original| location.portable_running(original, until))
                .context("canonical frontend is not the installed application")?,
        )
    };
    #[cfg(not(windows))]
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
            portable,
        };
        windows.check(until)?;
        windows
    };
    ensure!(
        Instant::now() < until && caller.current() && frontend.current(),
        "frontend changed during authentication"
    );
    let ownership_origin = if matches!(
        envelope.request,
        Request::DesktopOwnership { .. }
            | Request::DesktopOwnershipObserve { .. }
            | Request::WorktreeProgramStop { .. }
            | Request::WorktreeProgramCleanup { .. }
    ) {
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
        application: reply.hello.application,
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
    if let Request::WorktreeProgramStop {intent} = &envelope.request {
        ensure!(reply.request.ownership_mode==Some(intent.input.source.mode())
            && intent.input.manifest.configuration==configuration,
            "program-stop native mode/configuration changed");
    }
    if let Request::WorktreeProgramCleanup {intent} = &envelope.request {
        ensure!(reply.request.ownership_mode==Some(intent.input.stop.source.mode())
            && intent.input.stop.manifest.configuration==configuration,
            "program-cleanup native mode/configuration changed");
    }
    if let Request::DesktopOwnership { action, .. } = &envelope.request {
        ensure!(
            reply.request.ownership_mode == Some(action.mode()),
            "ownership mode does not match the native frontend"
        );
    }
    if let Request::DesktopOwnershipObserve { query } = &envelope.request {
        ensure!(reply.request.ownership_mode == Some(zerus_client_core::desktop::ownership::Mode::Service)
            && query.configuration == configuration,
            "ownership observation mode/configuration changed");
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
    if let Request::CreationWorktree { intent } = &envelope.request {
        ensure!(
            reply.request.ownership_mode
                == Some(zerus_client_core::desktop::ownership::Mode::Service)
                && intent.scope.configuration == configuration,
            "creation does not match the native service frontend"
        );
    }
    if let Request::CreationLaunch { intent } = &envelope.request {
        ensure!(
            reply.request.ownership_mode
                == Some(zerus_client_core::desktop::ownership::Mode::Service)
                && intent.scope.configuration == configuration,
            "creation does not match the native service frontend"
        );
    }
    if let Request::TaskMessage { intent } = &envelope.request {
        ensure!(
            reply.request.ownership_mode
                == Some(zerus_client_core::desktop::ownership::Mode::Service)
                && intent.scope.configuration == configuration,
            "task message does not match the native service frontend"
        );
    }
    if let Request::TaskMessageInputRelease { release } = &envelope.request {
        ensure!(
            reply.request.ownership_mode
                == Some(zerus_client_core::desktop::ownership::Mode::Service)
                && release.scope.configuration == configuration,
            "task message input release does not match the native service frontend"
        );
    }
    if let Request::DeferredSend { intent } = &envelope.request {
        ensure!(
            reply.request.ownership_mode
                == Some(zerus_client_core::desktop::ownership::Mode::Service)
                && intent.scope.configuration == configuration,
            "deferred send does not match the native service frontend"
        );
    }
    if let Request::DeferredAnswer { intent } = &envelope.request {
        ensure!(
            reply.request.ownership_mode
                == Some(zerus_client_core::desktop::ownership::Mode::Service)
                && intent.scope.configuration == configuration,
            "deferred answer does not match the native service frontend"
        );
    }
    if let Request::CompactToSend { intent } = &envelope.request {
        ensure!(
            reply.request.ownership_mode
                == Some(zerus_client_core::desktop::ownership::Mode::Service)
                && intent.scope.configuration == configuration,
            "compact-to-send does not match the native service frontend"
        );
    }
    if let Request::SessionResume { intent } = &envelope.request {
        ensure!(
            reply.request.ownership_mode
                == Some(zerus_client_core::desktop::ownership::Mode::Service)
                && intent.scope.configuration == configuration,
            "session resume does not match the native service frontend"
        );
    }
    if let Request::TaskStartReconcile { intent } = &envelope.request {
        ensure!(
            reply.request.ownership_mode
                == Some(zerus_client_core::desktop::ownership::Mode::Service)
                && intent.scope.configuration == configuration,
            "task reconciliation does not match the native service frontend"
        );
    }
    if let Request::TerminationOutcome { intent } = &envelope.request {
        ensure!(
            reply.request.ownership_mode
                == Some(zerus_client_core::desktop::ownership::Mode::Service)
                && intent.scope.configuration == configuration,
            "termination outcome does not match the native service frontend"
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
    WorktreeProgramStop,
    WorktreeProgramCleanup,
    TerminationOutcome,
    CacheMaintenance,
    CreationClone,
    CreationWorktree,
    CreationLaunch,
    TaskMessage,
    TaskMessageInputRelease,
    DeferredSend,
    DeferredAnswer,
    CompactToSend,
    SessionResume,
    TaskStartReconcile,
    ActivationWait,
    AutomaticApprovals,
    QuestionRoutes,
    QuestionRouteChild,
    MaintenanceConfigure,
    ImportPendingReads,
    DesktopOwnership,
    DesktopOwnershipObserve,
    NotificationPreferences,
    NotificationFocus,
}
impl From<&Request> for OrderedOperation {
    fn from(request: &Request) -> Self {
        use zerus_client_core::desktop::notification_control::Command;
        match request {
            Request::CacheMaintenance { .. } => Self::CacheMaintenance,
            Request::TerminationOutcome { .. } => Self::TerminationOutcome,
            Request::WorktreeProgramStop { .. } => Self::WorktreeProgramStop,
            Request::WorktreeProgramCleanup { .. } => Self::WorktreeProgramCleanup,
            Request::CreationClone { .. } => Self::CreationClone,
            Request::CreationWorktree { .. } => Self::CreationWorktree,
            Request::CreationLaunch { .. } => Self::CreationLaunch,
            Request::TaskMessage { .. } => Self::TaskMessage,
            Request::TaskMessageInputRelease { .. } => Self::TaskMessageInputRelease,
            Request::DeferredSend { .. } => Self::DeferredSend,
            Request::DeferredAnswer { .. } => Self::DeferredAnswer,
            Request::CompactToSend { .. } => Self::CompactToSend,
            Request::SessionResume { .. } => Self::SessionResume,
            Request::TaskStartReconcile { .. } => Self::TaskStartReconcile,
            Request::ActivationWait { .. } => Self::ActivationWait,
            Request::AutomaticApprovals { .. } => Self::AutomaticApprovals,
            Request::QuestionRoutes { .. } => Self::QuestionRoutes,
            Request::QuestionRouteChild { .. } => Self::QuestionRouteChild,
            Request::MaintenanceConfigure { .. } => Self::MaintenanceConfigure,
            Request::ImportPendingReads { .. } => Self::ImportPendingReads,
            Request::DesktopOwnership { .. } => Self::DesktopOwnership,
            Request::DesktopOwnershipObserve { .. } => Self::DesktopOwnershipObserve,
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
            b"HGS_SESSION_ID=",
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
    fn program_stop_intent() -> zerus_client_core::worktree_programs::invocation::Intent {
        use zerus_client_core::{
            termination_cleanup::Capture,
            worktree_programs::{self as p, stop},
            Target,
        };
        let capture = Capture {
            target: Target {
                computer: String::new(),
                name: "codex/example/original".into(),
                run_id: id(2),
                conversation_id: id(3),
            },
            machine: id(4),
            root: "/srv/example/work".into(),
            common_dir: "/srv/example/main/.git".into(),
            owner_id: id(5),
        };
        let original = String::from_utf8(capture.invocation(&id(6)).unwrap().stdin.unwrap()).unwrap();
        let input = stop::Input {
            request_id: id(7),
            confirmation_id: id(8),
            source: stop::Source::Service { epoch: id(9) },
            manifest: p::Manifest {
                abi: p::ABI.into(),
                id: id(10),
                capture,
                configuration: "a".repeat(64),
                boot: id(11),
                checkout: p::DirectoryId {
                    device: 1,
                    inode: 2,
                    mount: "fixture-object".into(),
                },
                issued: 1000,
                expires: 1300,
                programs: vec![],
            },
            original,
            archive_id: id(12),
            stop: stop::Kind::Normal,
        };
        let intent = p::invocation::Intent {
            input: Box::new(input),
            pipeline: id(13),
            socket: "/tmp/example/pins".into(),
            holder: p::custody::NativeChild {
                pid: 50,
                parent: 51,
                birth: "original-holder".into(),
                audit: None,
            },
        };
        assert!(intent.valid());
        intent
    }
    #[test]
    fn program_stop_data_is_isolated_from_generic_routes_and_binds_native_mode_and_scope() {
        let (mut envelope, caller, frontend, mut reply) = fixture();
        envelope.request = Request::WorktreeProgramStop {
            intent: Box::new(program_stop_intent()),
        };
        reply.request.request_id = envelope.request.id().into();
        reply.request.fingerprint = envelope.request.fingerprint();
        reply.request.ownership_mode = Some(zerus_client_core::desktop::ownership::Mode::Service);
        validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).unwrap();
        assert_eq!(
            OrderedOperation::from(&envelope.request),
            OrderedOperation::WorktreeProgramStop
        );
        assert_ne!(
            OrderedOperation::WorktreeProgramStop,
            OrderedOperation::TerminationOutcome
        );
        assert!(
            Validated::fixture(&serde_json::to_value(&envelope.request).unwrap())
                .into_request()
                .is_err()
        );
        let location = Scope {
            location: Location {
                root: "/fixture".into(),
                configuration: "a".repeat(64),
            },
            frontend_images: Vec::new(),
            portable: None,
            #[cfg(target_os = "linux")]
            sealed_reference: None,
        };
        let error = authenticate(
            &location,
            u32::MAX,
            &serde_json::to_value(&envelope).unwrap(),
        )
        .err()
        .expect("generic route refuses before PID lookup");
        assert!(error.to_string().contains("closed original helper"));
        for field in [
            "attempt",
            "serial",
            "child_pid",
            "child_birth",
            "request_id",
            "fingerprint",
        ] {
            let mut changed = Reply {
                hello: reply.hello.clone(),
                request: reply.request.clone(),
            };
            match field {
                "attempt" => changed.request.attempt = id(99),
                "serial" => changed.request.serial = 0,
                "child_pid" => changed.request.child_pid = caller.pid + 1,
                "child_birth" => changed.request.child_birth = "different".into(),
                "request_id" => changed.request.request_id = id(99),
                _ => changed.request.fingerprint = "b".repeat(64),
            }
            assert!(
                validate(&envelope, &caller, &frontend, &changed, &"a".repeat(64)).is_err(),
                "{field}"
            );
        }
        reply.request.ownership_mode = Some(zerus_client_core::desktop::ownership::Mode::Native);
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        let Request::WorktreeProgramStop { intent } = &mut envelope.request else {
            unreachable!()
        };
        intent.input.source = zerus_client_core::worktree_programs::stop::Source::Native {
            request_id: id(20),
            journal: None,
            revision: 0,
        };
        reply.request.fingerprint = envelope.request.fingerprint();
        validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).unwrap();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"b".repeat(64)).is_err());
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn final_cleanup_requires_its_own_exact_original_child_proof_and_reservation() {
        use zerus_client_core::worktree_programs::cleanup;
        let original = program_stop_intent();
        let intent = cleanup::Intent {
            input: Box::new(cleanup::Input { request_id: id(100), stop: original.input.clone() }),
            pipeline: original.pipeline.clone(), holder: original.holder.clone(), socket: original.socket.clone(),
        };
        assert!(intent.valid());
        let (_state, reservation) = crate::state::ProgramRemovalClaim::fixture(*intent.input.clone());
        let request = Request::WorktreeProgramCleanup { intent: Box::new(intent.clone()) };
        let admission = || ProgramCleanupAdmission(Validated::fixture(&serde_json::to_value(&request).unwrap()));
        let until = Instant::now() + Duration::from_secs(10);
        let location = Location { root: "/fixture".into(), configuration: "a".repeat(64) };
        assert!(Validated::fixture(&serde_json::to_value(&request).unwrap()).into_request().is_err());
        let generic_scope = Scope { location: Location { root: location.root.clone(), configuration: location.configuration.clone() },
            frontend_images: Vec::new(), portable: None, #[cfg(target_os = "linux")] sealed_reference: None };
        assert!(authenticate(&generic_scope, std::process::id(), &serde_json::to_value(Envelope {
            abi: zerus_client_core::desktop::ui_requests::ABI.into(), attempt: id(101), request: request.clone(),
        }).unwrap()).is_err());
        for change in 0..4 {
            let mut changed = intent.clone();
            match change { 0 => changed.input.request_id = id(102), 1 => changed.input.stop.source = zerus_client_core::worktree_programs::stop::Source::Service { epoch: id(102) },
                2 => changed.pipeline = id(102), _ => changed.holder.birth = "replacement".into() }
            assert!(admission().consume(&changed, until, &location, &reservation).is_err());
        }
        let wrong = ProgramCleanupAdmission(Validated::fixture(&serde_json::to_value(Request::WorktreeProgramStop { intent: Box::new(original) }).unwrap()));
        assert!(wrong.consume(&intent, until, &location, &reservation).is_err());
        let expired = ProgramCleanupPermit { intent: intent.clone(), life: ProgramLife::Fixture, until: Instant::now(), reservation: &reservation };
        assert!(expired.current().is_err());
        let permit = admission().consume(&intent, until, &location, &reservation).unwrap();
        permit.current().unwrap();
        assert!(admission().consume(&intent, until, &location, &reservation).is_err(), "a new proof cannot renew/reuse this consumed original");
        drop(permit);
        // Synthetic proof/data only, not native auth, consent or deletion.
    }
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn original_program_stop_proof_is_consumed_for_one_exact_bounded_operation() {
        // This isolates the typed admission with synthetic native evidence.
        // It is not a canonical frontend or selected-image positive.
        let mut intent = program_stop_intent();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        intent.input.manifest.issued = now;
        intent.input.manifest.expires = now + 300;
        let (state, reservation) = crate::state::ProgramStopClaim::fixture((*intent.input).clone());
        let request = Request::WorktreeProgramStop {
            intent: Box::new(intent.clone()),
        };
        let admission =
            || ProgramStopAdmission(Validated::fixture(&serde_json::to_value(&request).unwrap()));
        let until = Instant::now() + Duration::from_secs(10);
        let location = Location {
            root: "/fixture".into(),
            configuration: "a".repeat(64),
        };
        for change in 0..5 {
            let mut wrong = intent.clone();
            match change {
                0 => wrong.input.request_id = id(99),
                1 => wrong.input.confirmation_id = id(99),
                2 => {
                    wrong.input.source =
                        zerus_client_core::worktree_programs::stop::Source::Service { epoch: id(99) }
                }
                3 => wrong.pipeline = id(99),
                _ => wrong.holder.birth = "another-original".into(),
            }
            assert!(admission()
                .consume(&wrong, until, &location, &reservation)
                .is_err());
        }
        assert!(admission()
            .consume(&intent, Instant::now(), &location, &reservation)
            .is_err());
        assert!(admission()
            .consume(
                &intent,
                Instant::now() + Duration::from_secs(16),
                &location,
                &reservation
            )
            .is_err());
        let mut changed = intent.clone();
        changed.input.source =
            zerus_client_core::worktree_programs::stop::Source::Service { epoch: id(99) };
        let changed_request = Request::WorktreeProgramStop {
            intent: Box::new(changed.clone()),
        };
        let changed_admission = ProgramStopAdmission(Validated::fixture(
            &serde_json::to_value(changed_request).unwrap(),
        ));
        let error = changed_admission
            .consume(&changed, until, &location, &reservation)
            .err()
            .expect("fresh proof cannot rebind an old reservation");
        assert!(error.to_string().contains("reservation changed"));
        let denied = ProgramStopAdmission(Validated::fixture(
            &serde_json::to_value(Request::AutomaticApprovals {
                request_id: id(99),
                enabled: false,
                categories: None,
            })
            .unwrap(),
        ));
        assert!(denied
            .consume(&intent, until, &location, &reservation)
            .is_err());
        let expired = ProgramStopPermit {
            intent: intent.clone(),
            life: ProgramLife::Fixture,
            until: Instant::now(),
            reservation: &reservation,
        };
        assert!(expired.current().is_err());
        // Once admission has moved into a permit there is no Validated/cache
        // left to reconsume. Expiry can never be cleared by a status lookup.
        let mut native = live_authorized(request.clone(), 1)
            .into_program_stop(&mut Order::default())
            .unwrap();
        if let Freshness::Native { until, .. } = &mut native.0 .1 {
            *until = Instant::now();
        }
        assert!(
            native
                .consume(&intent, until, &location, &reservation)
                .is_err(),
            "expired native proof must refuse even with a future operation bound"
        );
        let permit = admission()
            .consume(&intent, until, &location, &reservation)
            .unwrap();
        assert_eq!(permit.intent(), &intent);
        permit.current().unwrap();
        let duplicate = admission()
            .consume(&intent, until, &location, &reservation)
            .err()
            .expect("the original claim cannot be consumed twice");
        assert!(duplicate.to_string().contains("already consumed"));
        std::fs::remove_file(
            state
                .path()
                .join("worktree-program-stops")
                .join(format!("{}.original.json", intent.input.request_id)),
        )
        .unwrap();
        assert!(
            permit.current().is_err(),
            "losing the original reservation closes the live permit too"
        );
    }
    fn fixture() -> (Envelope, Process, Process, Reply) {
        let envelope = Envelope {
            abi: ABI.into(),
            attempt: id(1),
            request: Request::AutomaticApprovals {
                request_id: id(2),
                enabled: true,
                categories: None,
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
            #[cfg(target_os = "linux")]
            linux_freshness: None,
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
            #[cfg(target_os = "linux")]
            linux_freshness: None,
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
    fn creation_worktree_has_its_own_lane_and_never_passes_the_generic_boundary() {
        use zerus_client_core::desktop::{
            creation_worktree::{Intent, Scope},
            ownership::Mode,
        };
        let (mut envelope, caller, frontend, mut reply) = fixture();
        let original = serde_json::json!({"id":id(80),"creation_schema":1,"created":1,"revision":4,
            "cmd":"codex","tag":"example","host":"","state":"worktree","auto":true,
            "parent":"/srv","target":"/srv/repo","branch":"zerus-example",
            "destination":"/srv/repo-zerus-example","worktree_common":"/srv/repo/.git",
            "worktree_id":id(2),"worktree_receipts":true,"expected_machine":id(81)});
        envelope.request = Request::CreationWorktree {
            intent: Box::new(Intent {
                scope: Scope {
                    configuration: "a".repeat(64),
                    service_epoch: id(91),
                },
                creation_id: id(80),
                worktree_request_id: id(2),
                revision: 4,
                original: original.to_string(),
            }),
        };
        assert_eq!(
            OrderedOperation::from(&envelope.request),
            OrderedOperation::CreationWorktree
        );
        assert_ne!(
            OrderedOperation::CreationWorktree,
            OrderedOperation::CreationClone
        );
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Native);
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Service);
        validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).unwrap();
        // Only a future typed actor accessor may consume it.
        assert!(Validated::fixture(&serde_json::json!(envelope.request))
            .into_request()
            .is_err());
        if let Request::CreationWorktree { intent } = &mut envelope.request {
            intent.scope.configuration = "b".repeat(64);
        }
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
    }
    #[test]
    fn creation_launch_has_its_own_lane_and_never_passes_the_generic_boundary() {
        use zerus_client_core::desktop::{
            creation_launch::{Intent, Scope},
            ownership::Mode,
        };
        let (mut envelope, caller, frontend, mut reply) = fixture();
        let original = serde_json::json!({"id":id(80),"creation_schema":1,"created":1,"revision":6,
            "cmd":"codex","tag":"example","host":"","state":"launching","auto":false,
            "target":"/srv/repo","launch_id":id(2),"launch_dispatched":true,
            "expected_machine":id(81)});
        envelope.request = Request::CreationLaunch {
            intent: Box::new(Intent {
                scope: Scope {
                    configuration: "a".repeat(64),
                    service_epoch: id(91),
                },
                creation_id: id(80),
                launch_id: id(2),
                revision: 6,
                original: original.to_string(),
            }),
        };
        assert_eq!(
            OrderedOperation::from(&envelope.request),
            OrderedOperation::CreationLaunch
        );
        for other in [
            OrderedOperation::CreationClone,
            OrderedOperation::CreationWorktree,
            OrderedOperation::TaskStartReconcile,
        ] {
            assert_ne!(OrderedOperation::CreationLaunch, other);
        }
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Native);
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Service);
        validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).unwrap();
        // Only a future typed actor accessor may consume it.
        assert!(Validated::fixture(&serde_json::json!(envelope.request))
            .into_request()
            .is_err());
        if let Request::CreationLaunch { intent } = &mut envelope.request {
            intent.scope.configuration = "b".repeat(64);
        }
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
    }
    #[test]
    fn task_message_has_its_own_lane_and_never_passes_the_generic_boundary() {
        use zerus_client_core::desktop::{
            ownership::Mode,
            task_message::{Intent, Scope},
        };
        use zerus_client_core::tasks_write::{input_digest, message_input, message_intent};
        let (mut envelope, caller, frontend, mut reply) = fixture();
        let intent = message_intent("hello", &[], "queue", "");
        let original = serde_json::json!({"id": id(2), "intent": intent,
            "input_sha256": input_digest(&message_input(&intent))});
        envelope.request = Request::TaskMessage {
            intent: Box::new(Intent {
                scope: Scope {
                    configuration: "a".repeat(64),
                    service_epoch: id(91),
                },
                project: "example".into(),
                task_id: id(80),
                request_id: id(2),
                original: original.to_string(),
                form: Default::default(),
            }),
        };
        assert_eq!(
            OrderedOperation::from(&envelope.request),
            OrderedOperation::TaskMessage
        );
        for other in [
            OrderedOperation::CreationLaunch,
            OrderedOperation::TaskStartReconcile,
            OrderedOperation::TerminationOutcome,
        ] {
            assert_ne!(OrderedOperation::TaskMessage, other);
        }
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Native);
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Service);
        validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).unwrap();
        // Only a future typed actor accessor may consume it.
        assert!(Validated::fixture(&serde_json::json!(envelope.request))
            .into_request()
            .is_err());
        if let Request::TaskMessage { intent } = &mut envelope.request {
            intent.scope.configuration = "b".repeat(64);
        }
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
    }
    #[test]
    fn task_message_input_release_has_its_own_lane_and_never_passes_the_generic_boundary() {
        use zerus_client_core::desktop::{
            ownership::Mode,
            task_message::{review::Release, Scope},
        };
        let (mut envelope, caller, frontend, mut reply) = fixture();
        envelope.request = Request::TaskMessageInputRelease {
            release: Box::new(Release {
                scope: Scope {
                    configuration: "a".repeat(64),
                    service_epoch: id(91),
                },
                release_id: id(3),
                request_id: id(2),
                state: "orphaned".into(),
                bytes: 10,
                input_sha256: "e".repeat(64),
                observed: None,
            }),
        };
        assert_eq!(
            OrderedOperation::from(&envelope.request),
            OrderedOperation::TaskMessageInputRelease
        );
        assert_ne!(
            OrderedOperation::TaskMessageInputRelease,
            OrderedOperation::TaskMessage
        );
        assert_eq!(envelope.request.id(), id(3), "the release's own identity");
        reply.request.request_id = id(3);
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Native);
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Service);
        validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).unwrap();
        assert!(Validated::fixture(&serde_json::json!(envelope.request))
            .into_request()
            .is_err());
        if let Request::TaskMessageInputRelease { release } = &mut envelope.request {
            release.scope.configuration = "b".repeat(64);
        }
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
    }
    /// A valid deferred answer to a Claude folder-trust question, built as the
    /// client builds it.
    fn deferred_answer_intent(
        configuration: String,
        epoch: String,
    ) -> zerus_client_core::desktop::deferred_answer::Intent {
        use zerus_client_core::{
            desktop::deferred_answer::{Intent, Scope},
            operation_recovery::deferred_input as checkpoint,
            questions::{consent_identity, AnswerItem},
        };
        let question = serde_json::json!({"question_id": "q-1", "question_hash": "ab".repeat(32),
            "run_id": "run-1", "conversation_id": "conversation-1", "source": "claude_folder_trust",
            "answer_transport": "claude_tui", "questions": [{"id": "q-1", "question": "Trust?",
                "options": [{"id": "yes", "label": "Yes"}]}]});
        let answers = vec![AnswerItem::Choice {
            question_id: "q-1".into(),
            selected_option_ids: vec!["yes".into()],
            text: String::new(),
        }];
        let identity = consent_identity(&question);
        let reserved = checkpoint::Checkpoint::reserve(checkpoint::Intent {
            kind: checkpoint::Kind::Answer,
            draft_revision: 1,
            message_sha256: checkpoint::message_sha256(
                &serde_json::to_string(&answers).unwrap(),
                std::iter::empty(),
            ),
            run_id: "run-1".into(),
            conversation_id: "conversation-1".into(),
            question: Some(checkpoint::Question {
                id: "q-1".into(),
                identity_sha256: checkpoint::identity_sha256(&identity),
            }),
            start: None,
            retry_of: None,
            consent: false,
        });
        let original = serde_json::json!({"request_id": reserved.request_id, "question_id": "q-1",
            "expected_question_hash": "ab".repeat(32), "expected_run_id": "run-1",
            "expected_conversation_id": "conversation-1", "answers": answers});
        Intent {
            scope: Scope {
                configuration,
                service_epoch: epoch,
            },
            session: "claude/private-answer".into(),
            request_id: reserved.request_id.clone(),
            checkpoint: serde_json::to_string(&reserved).unwrap(),
            pinned: identity.to_string(),
            original: original.to_string(),
        }
    }

    #[test]
    fn deferred_answer_has_its_own_lane_and_never_passes_the_generic_boundary() {
        use zerus_client_core::desktop::ownership::Mode;
        let (mut envelope, caller, frontend, mut reply) = fixture();
        let intent = deferred_answer_intent("a".repeat(64), id(91));
        envelope.request = Request::DeferredAnswer {
            intent: Box::new(intent.clone()),
        };
        assert_eq!(
            OrderedOperation::from(&envelope.request),
            OrderedOperation::DeferredAnswer
        );
        for other in [
            OrderedOperation::DeferredSend,
            OrderedOperation::TaskMessage,
            OrderedOperation::CreationLaunch,
        ] {
            assert_ne!(OrderedOperation::DeferredAnswer, other);
        }
        assert_eq!(envelope.request.id(), intent.request_id);
        reply.request.request_id = intent.request_id.clone();
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Native);
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Service);
        validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).unwrap();
        // Only a future typed actor accessor may consume it.
        assert!(Validated::fixture(&serde_json::json!(envelope.request))
            .into_request()
            .is_err());
        if let Request::DeferredAnswer { intent } = &mut envelope.request {
            intent.scope.configuration = "b".repeat(64);
        }
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
    }

    /// A valid compact-to-send, built as the client builds it.
    fn compact_to_send_intent(
        configuration: String,
        epoch: String,
    ) -> zerus_client_core::desktop::compact_to_send::Intent {
        use zerus_client_core::{
            desktop::compact_to_send::{Intent, Scope},
            operation_recovery::deferred_input as checkpoint,
        };
        let reserved = checkpoint::Checkpoint::reserve_compact_to_send(checkpoint::Intent {
            kind: checkpoint::Kind::CompactToSend,
            draft_revision: 1,
            message_sha256: checkpoint::message_sha256("after compaction", std::iter::empty()),
            run_id: "run-1".into(),
            conversation_id: "conversation-1".into(),
            question: None,
            start: None,
            retry_of: None,
            consent: true,
        })
        .unwrap();
        let compaction = serde_json::json!({"request_id": reserved.request_id,
            "expected_run_id": "run-1", "expected_conversation_id": "conversation-1"});
        let original = serde_json::json!({"request_id": reserved.send_request_id,
            "text": "after compaction", "attachments": [], "expected_run_id": "run-1",
            "expected_conversation_id": "conversation-1", "expected_compaction_id": reserved.request_id});
        Intent {
            scope: Scope {
                configuration,
                service_epoch: epoch,
            },
            session: "claude/private-compact".into(),
            request_id: reserved.request_id.clone(),
            send_request_id: reserved.send_request_id.clone().unwrap(),
            checkpoint: serde_json::to_string(&reserved).unwrap(),
            compaction: compaction.to_string(),
            original: original.to_string(),
        }
    }

    #[test]
    fn compact_to_send_has_its_own_lane_and_never_passes_the_generic_boundary() {
        use zerus_client_core::desktop::ownership::Mode;
        let (mut envelope, caller, frontend, mut reply) = fixture();
        let intent = compact_to_send_intent("a".repeat(64), id(91));
        envelope.request = Request::CompactToSend {
            intent: Box::new(intent.clone()),
        };
        assert_eq!(
            OrderedOperation::from(&envelope.request),
            OrderedOperation::CompactToSend
        );
        for other in [
            OrderedOperation::DeferredSend,
            OrderedOperation::DeferredAnswer,
            OrderedOperation::TaskMessage,
            OrderedOperation::CreationLaunch,
        ] {
            assert_ne!(OrderedOperation::CompactToSend, other);
        }
        assert_eq!(envelope.request.id(), intent.request_id);
        reply.request.request_id = intent.request_id.clone();
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Native);
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Service);
        validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).unwrap();
        // Only a future typed actor accessor may consume it.
        assert!(Validated::fixture(&serde_json::json!(envelope.request))
            .into_request()
            .is_err());
        if let Request::CompactToSend { intent } = &mut envelope.request {
            intent.scope.configuration = "b".repeat(64);
        }
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
    }

    /// A valid session resume, built as the client builds it.
    fn session_resume_intent(
        configuration: String,
        epoch: String,
    ) -> zerus_client_core::desktop::session_resume::Intent {
        use zerus_client_core::{
            desktop::session_resume::{Intent, Scope},
            operation_recovery::deferred_input as checkpoint,
        };
        let reserved = checkpoint::Checkpoint::reserve_session_resume(checkpoint::Intent {
            kind: checkpoint::Kind::ResumeToSend,
            draft_revision: 1,
            message_sha256: checkpoint::message_sha256("after resume", std::iter::empty()),
            run_id: "run-1".into(),
            conversation_id: "conversation-1".into(),
            question: None,
            start: Some(checkpoint::Start::Resume),
            retry_of: None,
            consent: true,
        })
        .unwrap();
        let resume = serde_json::json!({"request_id": reserved.request_id, "action": "resume",
            "expected_run_id": "run-1", "expected_conversation_id": "conversation-1"});
        let original = serde_json::json!({"request_id": reserved.send_request_id,
            "text": "after resume", "attachments": [], "expected_run_id": "run-1",
            "expected_conversation_id": "conversation-1"});
        Intent {
            scope: Scope {
                configuration,
                service_epoch: epoch,
            },
            session: "claude/private-resume".into(),
            request_id: reserved.request_id.clone(),
            send_request_id: reserved.send_request_id.clone().unwrap(),
            checkpoint: serde_json::to_string(&reserved).unwrap(),
            resume: resume.to_string(),
            original: original.to_string(),
        }
    }

    #[test]
    fn session_resume_has_its_own_lane_and_never_passes_the_generic_boundary() {
        use zerus_client_core::desktop::ownership::Mode;
        let (mut envelope, caller, frontend, mut reply) = fixture();
        let intent = session_resume_intent("a".repeat(64), id(92));
        envelope.request = Request::SessionResume {
            intent: Box::new(intent.clone()),
        };
        assert_eq!(
            OrderedOperation::from(&envelope.request),
            OrderedOperation::SessionResume
        );
        for other in [
            OrderedOperation::CompactToSend,
            OrderedOperation::DeferredSend,
            OrderedOperation::DeferredAnswer,
            OrderedOperation::TaskMessage,
            OrderedOperation::CreationLaunch,
        ] {
            assert_ne!(OrderedOperation::SessionResume, other);
        }
        assert_eq!(envelope.request.id(), intent.request_id);
        reply.request.request_id = intent.request_id.clone();
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Native);
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Service);
        validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).unwrap();
        // Only its own typed actor accessor may consume it.
        assert!(Validated::fixture(&serde_json::json!(envelope.request))
            .into_request()
            .is_err());
        if let Request::SessionResume { intent } = &mut envelope.request {
            intent.scope.configuration = "b".repeat(64);
        }
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
    }

    #[test]
    fn deferred_send_has_its_own_lane_and_never_passes_the_generic_boundary() {
        use zerus_client_core::{
            desktop::{
                deferred_send::{Intent, Scope},
                ownership::Mode,
            },
            operation_recovery::deferred_input as checkpoint,
        };
        let (mut envelope, caller, frontend, mut reply) = fixture();
        let reserved = checkpoint::Checkpoint::reserve(checkpoint::Intent {
            kind: checkpoint::Kind::Send,
            draft_revision: 1,
            message_sha256: checkpoint::message_sha256("hello", std::iter::empty()),
            run_id: "run-1".into(),
            conversation_id: "conversation-1".into(),
            question: None,
            start: None,
            retry_of: None,
            consent: true,
        });
        let original = serde_json::json!({"request_id": reserved.request_id, "text": "hello",
            "attachments": [], "expected_run_id": "run-1",
            "expected_conversation_id": "conversation-1"});
        envelope.request = Request::DeferredSend {
            intent: Box::new(Intent {
                scope: Scope {
                    configuration: "a".repeat(64),
                    service_epoch: id(91),
                },
                session: "claude/private-send".into(),
                request_id: reserved.request_id.clone(),
                checkpoint: serde_json::to_string(&reserved).unwrap(),
                original: original.to_string(),
            }),
        };
        assert_eq!(
            OrderedOperation::from(&envelope.request),
            OrderedOperation::DeferredSend
        );
        for other in [
            OrderedOperation::TaskMessage,
            OrderedOperation::TaskMessageInputRelease,
            OrderedOperation::CreationLaunch,
        ] {
            assert_ne!(OrderedOperation::DeferredSend, other);
        }
        assert_eq!(envelope.request.id(), reserved.request_id);
        reply.request.request_id = reserved.request_id.clone();
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Native);
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Service);
        validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).unwrap();
        // Only a future typed actor accessor may consume it.
        assert!(Validated::fixture(&serde_json::json!(envelope.request))
            .into_request()
            .is_err());
        if let Request::DeferredSend { intent } = &mut envelope.request {
            intent.scope.configuration = "b".repeat(64);
        }
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
    }
    #[test]
    fn the_shared_frontend_line_is_exactly_what_the_child_sends() {
        // The wire's size check and the child's hello cannot drift apart.
        let configuration = "c".repeat(64);
        let envelope = serde_json::json!({"abi": "desktop-ui-request-v1", "attempt": id(5),
            "request": {"op": "activation_wait", "request_id": id(6)},
            "unicode": "пример \u{1b} \"quoted\""});
        assert_eq!(
            super::super::protocol::encode(&serde_json::json!({"abi": super::super::protocol::ABI,
                "configuration": configuration, "op": "frontend_request", "request": envelope}))
            .unwrap(),
            zerus_client_core::desktop::ui_requests::frontend_line(&configuration, &envelope)
        );
    }
    #[test]
    fn task_start_reconcile_is_native_service_bound_and_excluded_from_generic_requests() {
        use zerus_client_core::desktop::{
            ownership::Mode,
            task_start_reconcile::{Intent, Scope},
        };
        let (mut envelope, caller, frontend, mut reply) = fixture();
        let fixture: Value = serde_json::from_str(include_str!(
            "../../client/fixtures/desktop-operation-port-v1.json"
        ))
        .unwrap();
        let mut record: Value =
            serde_json::from_str(fixture["raw_record"].as_str().unwrap()).unwrap();
        record["intent"]["retained_input"] = fixture["retained_input"].clone();
        let original = record.to_string();
        let mut binding: zerus_client_core::operation_recovery::task_start::Binding =
            serde_json::from_value(fixture["binding"].clone()).unwrap();
        binding.key.configuration = "a".repeat(64);
        binding.source_record_sha256 = zerus_desktop_migration::sha256(original.as_bytes());
        binding.ui_snapshot_sha256 = zerus_client_core::tasks_write::outbox::snapshot(&record);
        let intent = Intent {
            scope: Scope {
                configuration: "a".repeat(64),
                service_epoch: id(91),
            },
            reconcile_id: id(83),
            binding,
            original,
        };
        intent.clone().verify().unwrap();
        envelope.request = Request::TaskStartReconcile {
            intent: Box::new(intent.clone()),
        };
        assert_eq!(
            OrderedOperation::from(&envelope.request),
            OrderedOperation::TaskStartReconcile
        );
        assert_ne!(
            OrderedOperation::TaskStartReconcile,
            OrderedOperation::CreationClone
        );
        assert_ne!(
            OrderedOperation::TaskStartReconcile,
            OrderedOperation::CreationWorktree
        );
        reply.request.request_id = envelope.request.id().into();
        reply.request.fingerprint = envelope.request.fingerprint();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Native);
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.ownership_mode = Some(Mode::Service);
        validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).unwrap();
        reply.request.request_id = intent.binding.key.request_id.clone();
        assert!(validate(&envelope, &caller, &frontend, &reply, &"a".repeat(64)).is_err());
        reply.request.request_id = intent.reconcile_id.clone();
        assert!(Validated::fixture(&serde_json::json!(envelope.request))
            .into_request()
            .is_err());
        assert_eq!(
            Validated::fixture(&serde_json::json!(envelope.request))
                .task_start_reconcile()
                .unwrap(),
            intent
        );
        let expired = Validated(
            envelope.request.clone(),
            Freshness::Native {
                caller: caller.clone(),
                frontend: frontend.clone(),
                until: Instant::now(),
                #[cfg(windows)]
                windows: None,
            },
            Metadata::default(),
        );
        assert!(expired.task_start_reconcile().is_err());
        #[cfg(unix)]
        {
            let mut order = Order::default();
            live_authorized(envelope.request.clone(), 7)
                .into_request(&mut order)
                .unwrap()
                .task_start_reconcile()
                .unwrap();
            assert!(live_authorized(envelope.request.clone(), 7)
                .into_request(&mut order)
                .is_err());
        }
        if let Request::TaskStartReconcile { intent } = &mut envelope.request {
            intent.scope.configuration = "b".repeat(64);
            intent.binding.key.configuration = "b".repeat(64);
            intent.as_ref().clone().verify().unwrap();
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
    #[cfg(unix)]
    fn the_mac_transport_prefix_keeps_exact_frontend_request_arguments() {
        let args = |words: &[&str]| words.iter().map(|word| (*word).into()).collect::<Vec<String>>();
        let original = args(&["desktop", "frontend-request", "--json"]);
        assert!(frontend_request_arguments(&original, false));
        assert!(frontend_request_arguments(&original, true));
        let mut gated = original.clone();
        gated.insert(0, zerus_client_core::run::rendezvous::ARG.into());
        assert!(frontend_request_arguments(&gated, true));
        assert!(!frontend_request_arguments(&gated, false));
        for words in [
            vec!["--desktop-helper-rendezvous-v1", "desktop", "request", "--json"],
            vec!["--desktop-helper-rendezvous-v1", "desktop", "frontend-request", "--json", "extra"],
            vec!["--desktop-helper-rendezvous-v1", "--desktop-helper-rendezvous-v1", "desktop", "frontend-request", "--json"],
            vec!["desktop", "--desktop-helper-rendezvous-v1", "frontend-request", "--json"],
            vec!["--desktop-helper-rendezvous-v2", "desktop", "frontend-request", "--json"],
        ] {
            assert!(!frontend_request_arguments(&args(&words), true));
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
            application: "com.hgdev.hgs-tray".into(),
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
                categories: None,
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
    fn termination_outcome_has_an_isolated_exhaustive_native_lane() {
        use zerus_client_core::{
            desktop::termination_outcome::{Choice, Intent, Scope},
            termination_cleanup::Capture,
            Target,
        };
        let capture = Capture {
            target: Target {
                computer: String::new(),
                name: "codex/example/source".into(),
                run_id: "run".into(),
                conversation_id: "conversation".into(),
            },
            machine: id(7),
            root: "/srv/example/work".into(),
            common_dir: "/srv/example/main/.git".into(),
            owner_id: "original-owner".into(),
        };
        let original =
            String::from_utf8(capture.invocation(&id(92)).unwrap().stdin.unwrap()).unwrap();
        let request = Request::TerminationOutcome {
            intent: Box::new(Intent {
                scope: Scope {
                    configuration: "a".repeat(64),
                    service_epoch: id(93),
                },
                confirmation_id: id(94),
                choice: Choice::RemoveAfterTerminate,
                request_id: id(92),
                capture,
                original,
            }),
        };
        let lane = OrderedOperation::from(&request);
        assert_eq!(lane, OrderedOperation::TerminationOutcome);
        let instance = (7, "100".into(), id(1));
        let mut order = Order::default();
        let now = Instant::now();
        order
            .observe(instance.clone(), OrderedOperation::CreationLaunch, 99, now)
            .unwrap();
        order.observe(instance.clone(), lane, 1, now).unwrap();
        assert!(order.observe(instance.clone(), lane, 1, now).is_err());
        order.observe(instance, lane, 2, now).unwrap();
        assert!(Validated::fixture(&serde_json::to_value(request).unwrap())
            .into_request()
            .is_err());
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
            portable: None,
            #[cfg(target_os = "linux")]
            sealed_reference: None,
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
            portable: None,
            #[cfg(target_os = "linux")]
            sealed_reference: None,
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
