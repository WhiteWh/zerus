//! Per-invocation native frontend evidence. A nonce or JSON role is not an
//! authority: the service must also bind its kernel peer to this original child.
//! Native policy handlers and the read-only activation consumer prepare/register
//! these closed requests; policy changes still require their confirmation UI.
use super::{Hello, Responder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io,
    process::Child,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};
use zerus_desktop_migration::canonical_uuid;

pub const ABI: &str = "desktop-ui-request-v1";
pub const HELP_MARKER: &str = "Desktop frontend request ABI: desktop-ui-request-v1";
pub const LIMIT: usize = 8;
pub const LIFETIME: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    WorktreeProgramStop {
        intent: Box<crate::worktree_programs::invocation::Intent>,
    },
    WorktreeProgramCleanup {
        intent: Box<crate::worktree_programs::cleanup::Intent>,
    },
    CreationClone {
        intent: Box<super::creation_clone::Intent>,
    },
    CreationWorktree {
        intent: Box<super::creation_worktree::Intent>,
    },
    CreationLaunch {
        intent: Box<super::creation_launch::Intent>,
    },
    TaskMessage {
        intent: Box<super::task_message::Intent>,
    },
    TaskMessageInputRelease {
        release: Box<super::task_message::review::Release>,
    },
    DeferredSend {
        intent: Box<super::deferred_send::Intent>,
    },
    DeferredAnswer {
        intent: Box<super::deferred_answer::Intent>,
    },
    CompactToSend {
        intent: Box<super::compact_to_send::Intent>,
    },
    SessionResume {
        intent: Box<super::session_resume::Intent>,
    },
    TaskStartReconcile {
        intent: Box<super::task_start_reconcile::Intent>,
    },
    TerminationOutcome {
        intent: Box<super::termination_outcome::Intent>,
    },
    NotificationControl {
        scope: super::notification_control::Scope,
        control: super::notification_control::Command,
    },
    DesktopOwnership {
        request_id: String,
        action: super::ownership::Action,
        journal: Option<String>,
        revision: u64,
    },
    DesktopOwnershipObserve {
        query: Box<super::ownership::observation::Query>,
    },
    ImportPendingReads {
        request_id: String,
        page: Box<super::pending_reads::Page>,
    },
    ActivationWait {
        request_id: String,
    },
    AutomaticApprovals {
        request_id: String,
        enabled: bool,
        /// The complete set of approval categories to keep on (zerus-sz42x),
        /// by ID. Absent, the request changes only the switch. Present, it
        /// changes only the categories and `enabled` must equal the
        /// committed switch, so a category change never turns approvals on.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        categories: Option<Vec<String>>,
    },
    /// The global switch of the owner-approved question route
    /// (zerus-sa9qn.32): the person's own choice, through the native UI
    /// permit only. Off stops new route reservations at once; it never
    /// cancels or replays a reserved, sent or unknown route.
    QuestionRoutes {
        request_id: String,
        enabled: bool,
    },
    CacheMaintenance {
        request_id: String,
        command: Box<super::cache_maintenance::Command>,
    },
    MaintenanceConfigure {
        request_id: String,
        #[serde(deserialize_with = "required_option")]
        update: Option<Box<crate::maintenance::UpdateConfiguration>>,
        #[serde(deserialize_with = "required_option")]
        recovery: Option<Box<crate::maintenance::RecoveryConfiguration>>,
    },
}
fn required_option<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::deserialize(d)
}
impl Request {
    pub fn maintenance_configure(
        request_id: String,
        configuration: crate::maintenance::Configuration,
    ) -> Self {
        Self::MaintenanceConfigure {
            request_id,
            update: configuration.update.map(Box::new),
            recovery: configuration.recovery.map(Box::new),
        }
    }
    pub fn id(&self) -> &str {
        match self {
            Self::WorktreeProgramStop { intent } => intent.request_id(),
            Self::WorktreeProgramCleanup { intent } => intent.request_id(),
            Self::CreationClone { intent } => &intent.clone_request_id,
            Self::CreationWorktree { intent } => &intent.worktree_request_id,
            Self::CreationLaunch { intent } => &intent.launch_id,
            Self::TaskMessage { intent } => &intent.request_id,
            Self::TaskMessageInputRelease { release } => &release.release_id,
            Self::DeferredSend { intent } => &intent.request_id,
            Self::DeferredAnswer { intent } => &intent.request_id,
            Self::CompactToSend { intent } => &intent.request_id,
            Self::SessionResume { intent } => &intent.request_id,
            Self::TaskStartReconcile { intent } => &intent.reconcile_id,
            Self::TerminationOutcome { intent } => &intent.request_id,
            Self::NotificationControl { control, .. } => control.request_id(),
            Self::DesktopOwnershipObserve { query } => &query.request_id,
            Self::ActivationWait { request_id }
            | Self::DesktopOwnership { request_id, .. }
            | Self::ImportPendingReads { request_id, .. }
            | Self::AutomaticApprovals { request_id, .. }
            | Self::QuestionRoutes { request_id, .. }
            | Self::CacheMaintenance { request_id, .. }
            | Self::MaintenanceConfigure { request_id, .. } => request_id,
        }
    }
    pub fn operation(&self) -> &'static str {
        match self {
            Self::WorktreeProgramStop { .. } => "worktree_program_stop",
            Self::WorktreeProgramCleanup { .. } => "worktree_program_cleanup",
            Self::CreationClone { .. } => "creation_clone",
            Self::CreationWorktree { .. } => "creation_worktree",
            Self::CreationLaunch { .. } => "creation_launch",
            Self::TaskMessage { .. } => "task_message",
            Self::TaskMessageInputRelease { .. } => "task_message_input_release",
            Self::DeferredSend { .. } => "deferred_send",
            Self::DeferredAnswer { .. } => "deferred_answer",
            Self::CompactToSend { .. } => "compact_to_send",
            Self::SessionResume { .. } => "session_resume",
            Self::TaskStartReconcile { .. } => "task_start_reconcile",
            Self::TerminationOutcome { .. } => "termination_outcome",
            Self::NotificationControl { control, .. } => control.operation(),
            Self::DesktopOwnership { .. } => "desktop_ownership",
            Self::DesktopOwnershipObserve { .. } => "desktop_ownership_observe",
            Self::ImportPendingReads { .. } => "import_pending_reads",
            Self::ActivationWait { .. } => "activation_wait",
            Self::AutomaticApprovals { .. } => "automatic_approvals",
            Self::QuestionRoutes { .. } => "question_routes",
            Self::CacheMaintenance { .. } => "cache_maintenance",
            Self::MaintenanceConfigure { .. } => "maintenance_configure",
        }
    }
    fn valid(&self) -> bool {
        match self {
            Self::WorktreeProgramStop { intent } => intent.valid(),
            Self::WorktreeProgramCleanup { intent } => intent.valid(),
            Self::CreationClone { intent } => intent.as_ref().clone().verify().is_ok(),
            Self::CreationWorktree { intent } => intent.as_ref().clone().verify().is_ok(),
            Self::CreationLaunch { intent } => intent.as_ref().clone().verify().is_ok(),
            Self::TaskMessage { intent } => intent.as_ref().clone().verify().is_ok(),
            Self::TaskMessageInputRelease { release } => release.valid(),
            Self::DeferredSend { intent } => intent.as_ref().clone().verify().is_ok(),
            Self::DeferredAnswer { intent } => intent.as_ref().clone().verify().is_ok(),
            Self::CompactToSend { intent } => intent.as_ref().clone().verify().is_ok(),
            Self::SessionResume { intent } => intent.as_ref().clone().verify().is_ok(),
            Self::TaskStartReconcile { intent } => intent.as_ref().clone().verify().is_ok(),
            Self::TerminationOutcome { intent } => intent.valid(),
            Self::NotificationControl { scope, control } => scope.valid() && control.valid(),
            Self::DesktopOwnership {
                journal, revision, ..
            } => {
                journal.as_deref().is_none_or(canonical_uuid)
                    && *revision <= super::ownership::MAX_REVISION
                    && (journal.is_some() || *revision == 0)
            }
            Self::DesktopOwnershipObserve { query } => query.valid(),
            Self::ImportPendingReads { page, .. } => page.valid(),
            Self::ActivationWait { .. } => true,
            Self::AutomaticApprovals { categories, .. } => categories
                .as_deref()
                .is_none_or(|ids| crate::approvals::automatic::Set::from_ids(ids).is_some()),
            Self::QuestionRoutes { .. } => true,
            Self::CacheMaintenance {
                request_id,
                command,
            } => command.valid(request_id),
            Self::MaintenanceConfigure {
                update, recovery, ..
            } => crate::maintenance::Configuration {
                update: update.as_deref().cloned(),
                recovery: recovery.as_deref().cloned(),
            }
            .valid(),
        }
    }
    pub fn fingerprint(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).expect("fixed request fields"))
        )
    }
}

/// The complete line the `frontend-request` child sends the service for one
/// envelope: the `desktop-v1` hello around it and its newline. The service
/// reads at most [`zerus_watch_protocol::CONTROL_LINE_BYTES`] of it.
pub fn frontend_line(configuration: &str, envelope: &serde_json::Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&serde_json::json!({
        "abi": super::service::ABI,
        "configuration": configuration,
        "op": "frontend_request",
        "request": envelope,
    }))
    .expect("a JSON line");
    bytes.push(b'\n');
    bytes
}

/// Whether `request` fits that complete line for any attempt and
/// configuration: both have a fixed length, so the size is exact. A request
/// that does not fit must never be sent.
pub fn fits_frontend_line(request: &Request) -> bool {
    let envelope = Envelope {
        abi: ABI.into(),
        attempt: "00000000-0000-4000-8000-000000000000".into(),
        request: request.clone(),
    };
    serde_json::to_value(&envelope).is_ok_and(|envelope| {
        frontend_line(&"0".repeat(64), &envelope).len() <= zerus_watch_protocol::CONTROL_LINE_BYTES
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub abi: String,
    pub attempt: String,
    pub request: Request,
}
impl Envelope {
    pub fn valid(&self) -> bool {
        self.abi == ABI
            && canonical_uuid(&self.attempt)
            && canonical_uuid(self.request.id())
            && self.request.valid()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub attempt: String,
    pub request_id: String,
    pub fingerprint: String,
    pub child_pid: u32,
    pub child_birth: String,
    pub serial: u64,
    /// Cached native startup mode, emitted for ownership/cache intents. It is
    /// not an input field on Request and is checked by the service's original
    /// kernel-peer/cached-child proof before any ownership mutation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership_mode: Option<super::ownership::Mode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification: Option<super::notification_control::NativeControl>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub hello: Hello,
    pub request: Evidence,
}

/// A fixed 59-byte line, inside the existing instance listener's 64-byte cap.
pub fn query(attempt: &str) -> Option<Vec<u8>> {
    canonical_uuid(attempt).then(|| format!("{ABI} {attempt}\n").into_bytes())
}
pub(super) fn query_attempt(line: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(line).ok()?;
    let attempt = text.strip_prefix(ABI)?.strip_prefix(' ')?;
    canonical_uuid(attempt).then_some(attempt)
}

struct Slot {
    envelope: Envelope,
    until: Instant,
    child: Mutex<Option<(u32, String)>>,
    serial: u64,
    ownership_mode: Option<super::ownership::Mode>,
    notification: Option<super::notification_control::NativeControl>,
}

#[derive(Default)]
struct State {
    slots: BTreeMap<String, Weak<Slot>>,
    serial: u64,
}
#[derive(Default)]
pub(super) struct Cache(Mutex<State>);
impl Cache {
    #[cfg(test)]
    pub fn prepare(&self, envelope: Envelope) -> io::Result<Registration> {
        self.prepare_with_mode(envelope, None)
    }
    #[cfg(test)]
    fn prepare_with_mode(
        &self,
        envelope: Envelope,
        ownership_mode: Option<super::ownership::Mode>,
    ) -> io::Result<Registration> {
        self.prepare_with_native(envelope, ownership_mode, None)
    }
    fn prepare_with_native(
        &self,
        envelope: Envelope,
        ownership_mode: Option<super::ownership::Mode>,
        notification: Option<super::notification_control::NativeControl>,
    ) -> io::Result<Registration> {
        if !envelope.valid() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        if let Request::WorktreeProgramStop { intent } = &envelope.request {
            if ownership_mode != Some(intent.input.source.mode()) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        } else if let Request::WorktreeProgramCleanup { intent } = &envelope.request {
            if ownership_mode != Some(intent.input.stop.source.mode()) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        } else if let Request::DesktopOwnership { action, .. } = &envelope.request {
            if ownership_mode != Some(action.mode()) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        } else if matches!(
            envelope.request,
            Request::CacheMaintenance { .. }
                | Request::DesktopOwnershipObserve { .. }
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
        ) {
            if ownership_mode != Some(super::ownership::Mode::Service) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        } else if ownership_mode.is_some() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        match (&envelope.request, &notification) {
            (Request::NotificationControl { scope, control }, Some(native))
                if native.scope == *scope
                    && native.mode == super::ownership::Mode::Service
                    && match control {
                        super::notification_control::Command::Configure { .. } => {
                            native.focus.is_none()
                        }
                        super::notification_control::Command::Focus { .. } => {
                            native.focus.as_ref().is_some_and(|v| v.valid())
                        }
                    } => {}
            (Request::NotificationControl { .. }, _) | (_, Some(_)) => {
                return Err(io::ErrorKind::PermissionDenied.into())
            }
            _ => (),
        }
        let mut state = self.0.lock().map_err(|_| io::ErrorKind::Other)?;
        // Expiration revokes evidence but does not recycle a live resource slot.
        // The runner must retain Registration::resources with its original
        // child and all pipes, including incomplete exchanges.
        state.slots.retain(|_, slot| slot.strong_count() != 0);
        if state.slots.len() == LIMIT || state.slots.contains_key(&envelope.attempt) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        state.serial = state.serial.checked_add(1).ok_or(io::ErrorKind::Other)?;
        let slot = Arc::new(Slot {
            envelope,
            until: Instant::now() + LIFETIME,
            child: Mutex::new(None),
            serial: state.serial,
            ownership_mode,
            notification,
        });
        state
            .slots
            .insert(slot.envelope.attempt.clone(), Arc::downgrade(&slot));
        Ok(Registration(slot))
    }

    pub fn evidence(&self, attempt: &str) -> Option<Evidence> {
        self.evidence_at(attempt, Instant::now())
    }
    fn evidence_at(&self, attempt: &str, now: Instant) -> Option<Evidence> {
        let slot = self.0.try_lock().ok()?.slots.get(attempt)?.upgrade()?;
        if now >= slot.until {
            return None;
        }
        let (child_pid, child_birth) = slot.child.try_lock().ok()?.as_ref()?.clone();
        Some(Evidence {
            attempt: attempt.into(),
            request_id: slot.envelope.request.id().into(),
            fingerprint: slot.envelope.request.fingerprint(),
            child_pid,
            child_birth,
            serial: slot.serial,
            ownership_mode: slot.ownership_mode,
            notification: slot.notification.clone(),
        })
    }
}

/// Created before spawn; bind the returned original Child before returning it
/// to the maintained runner, which only then sends the JSON on stdin. A failed
/// identity read leaves no evidence but must still return the Child to its owner.
pub struct Registration(Arc<Slot>);
impl Registration {
    pub fn envelope(&self) -> &Envelope {
        &self.0.envelope
    }
    pub fn resources(&self) -> Arc<dyn Send + Sync> {
        self.0.clone()
    }
    pub fn notification_evidence(&self) -> Option<&super::notification_control::NativeControl> {
        self.0.notification.as_ref()
    }
    pub fn bind_original(&self, child: &Child) -> bool {
        let Some(birth) = child_birth(child) else {
            return false;
        };
        let Ok(mut binding) = self.0.child.lock() else {
            return false;
        };
        if binding.is_some() || Instant::now() >= self.0.until {
            return false;
        }
        *binding = Some((child.id(), birth));
        true
    }
}

impl Responder {
    /// Native confirmation code only. No request/role/confirmation Boolean from
    /// IPC can call this constructor. Service-mode startup still configures the
    /// existing canonical responder before accepting a policy interaction.
    pub fn prepare_ui_request(
        &self,
        attempt: String,
        request: Request,
    ) -> io::Result<Registration> {
        let agent = ["HGS_SESSION", "HGS_SESSION_ID", "HGS_RUN_ID"]
            .iter()
            .any(|key| std::env::var_os(key).is_some_and(|v| !v.is_empty()));
        if agent
            || !self.live
            || self.replay
            || self.application != "com.hgdev.hgs-tray"
            || self.configuration.get().is_none()
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let mode = match &request {
            Request::CacheMaintenance { .. } => {
                let mode = self.ownership_mode.get().copied();
                if mode != Some(super::ownership::Mode::Service) {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                mode
            }
            Request::CreationClone { intent } => {
                if self.ownership_mode.get() != Some(&super::ownership::Mode::Service)
                    || self.configuration.get() != Some(&intent.scope.configuration)
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Some(super::ownership::Mode::Service)
            }
            Request::CreationWorktree { intent } => {
                if self.ownership_mode.get() != Some(&super::ownership::Mode::Service)
                    || self.configuration.get() != Some(&intent.scope.configuration)
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Some(super::ownership::Mode::Service)
            }
            Request::CreationLaunch { intent } => {
                if self.ownership_mode.get() != Some(&super::ownership::Mode::Service)
                    || self.configuration.get() != Some(&intent.scope.configuration)
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Some(super::ownership::Mode::Service)
            }
            Request::TaskStartReconcile { intent } => {
                if self.ownership_mode.get() != Some(&super::ownership::Mode::Service)
                    || self.configuration.get() != Some(&intent.scope.configuration)
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Some(super::ownership::Mode::Service)
            }
            Request::WorktreeProgramStop { intent } => {
                let mode = intent.input.source.mode();
                if self.ownership_mode.get() != Some(&mode)
                    || self.configuration.get() != Some(&intent.input.manifest.configuration)
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Some(mode)
            }
            Request::WorktreeProgramCleanup { intent } => {
                let mode = intent.input.stop.source.mode();
                if self.ownership_mode.get() != Some(&mode)
                    || self.configuration.get() != Some(&intent.input.stop.manifest.configuration)
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Some(mode)
            }
            Request::TerminationOutcome { intent } => {
                if self.ownership_mode.get() != Some(&super::ownership::Mode::Service)
                    || self.configuration.get() != Some(&intent.scope.configuration)
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Some(super::ownership::Mode::Service)
            }
            Request::TaskMessage { intent } => {
                if self.ownership_mode.get() != Some(&super::ownership::Mode::Service)
                    || self.configuration.get() != Some(&intent.scope.configuration)
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Some(super::ownership::Mode::Service)
            }
            Request::TaskMessageInputRelease { release } => {
                if self.ownership_mode.get() != Some(&super::ownership::Mode::Service)
                    || self.configuration.get() != Some(&release.scope.configuration)
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Some(super::ownership::Mode::Service)
            }
            Request::DeferredSend { intent } => {
                if self.ownership_mode.get() != Some(&super::ownership::Mode::Service)
                    || self.configuration.get() != Some(&intent.scope.configuration)
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Some(super::ownership::Mode::Service)
            }
            Request::DeferredAnswer { intent } => {
                if self.ownership_mode.get() != Some(&super::ownership::Mode::Service)
                    || self.configuration.get() != Some(&intent.scope.configuration)
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Some(super::ownership::Mode::Service)
            }
            Request::CompactToSend { intent } => {
                if self.ownership_mode.get() != Some(&super::ownership::Mode::Service)
                    || self.configuration.get() != Some(&intent.scope.configuration)
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Some(super::ownership::Mode::Service)
            }
            Request::SessionResume { intent } => {
                if self.ownership_mode.get() != Some(&super::ownership::Mode::Service)
                    || self.configuration.get() != Some(&intent.scope.configuration)
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Some(super::ownership::Mode::Service)
            }
            Request::DesktopOwnership { action, .. } => {
                let mode = self.ownership_mode.get().copied();
                if mode != Some(action.mode()) {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                mode
            }
            Request::DesktopOwnershipObserve { query } => {
                if self.ownership_mode.get() != Some(&super::ownership::Mode::Service)
                    || self.configuration.get() != Some(&query.configuration)
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Some(super::ownership::Mode::Service)
            }
            _ => None,
        };
        let notification = if let Request::NotificationControl { scope, control } = &request {
            if self.ownership_mode.get() != Some(&super::ownership::Mode::Service) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Some(
                self.notification_focus
                    .evidence(
                        scope,
                        matches!(control, super::notification_control::Command::Focus { .. }),
                    )
                    .ok_or(io::ErrorKind::NotConnected)?,
            )
        } else {
            None
        };
        self.ui_requests.prepare_with_native(
            Envelope {
                abi: ABI.into(),
                attempt,
                request,
            },
            mode,
            notification,
        )
    }

    pub(super) fn ui_reply(&self, attempt: &str) -> Option<Vec<u8>> {
        let request = self.ui_requests.evidence(attempt)?;
        if let Some(native) = &request.notification {
            if self
                .notification_focus
                .evidence(&native.scope, native.focus.is_some())
                .as_ref()
                != Some(native)
            {
                return None; // Revoke stale evidence, never recycle its resource charge.
            }
        }
        let mut bytes = serde_json::to_vec(&Reply {
            hello: self.hello(attempt),
            request,
        })
        .ok()?;
        bytes.push(b'\n');
        (bytes.len() <= super::REPLY_BYTES).then_some(bytes)
    }
}

#[cfg(target_os = "linux")]
fn child_birth(child: &Child) -> Option<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(format!("/proc/{}/stat", child.id()))
        .ok()?
        .take(4097)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 4096 {
        return None;
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    let birth = text.rsplit_once(") ")?.1.split_whitespace().nth(19)?;
    (birth.len() <= 20 && birth.bytes().all(|b| b.is_ascii_digit())).then(|| birth.into())
}

#[cfg(target_os = "macos")]
fn child_birth(child: &Child) -> Option<String> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    // SAFETY: original child PID and exactly sized writable native structure.
    let used = unsafe {
        libc::proc_pidinfo(
            child.id() as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if used != size as i32 {
        return None;
    }
    // SAFETY: the full structure was initialized by proc_pidinfo.
    let info = unsafe { info.assume_init() };
    Some(format!(
        "{}:{}",
        info.pbi_start_tvsec, info.pbi_start_tvusec
    ))
}

#[cfg(windows)]
fn child_birth(child: &Child) -> Option<String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{Foundation::FILETIME, System::Threading::GetProcessTimes};
    let mut times = [FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    }; 4];
    // SAFETY: the original owned Child handle and four valid FILETIME outputs.
    if unsafe {
        GetProcessTimes(
            child.as_raw_handle(),
            &mut times[0],
            &mut times[1],
            &mut times[2],
            &mut times[3],
        )
    } == 0
    {
        return None;
    }
    Some(
        ((u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime))
            .to_string(),
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn child_birth(_: &Child) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Read,
        process::{Command, Stdio},
        thread,
    };

    fn id(n: usize) -> String {
        format!("00000000-0000-4000-8000-{n:012x}")
    }
    fn envelope(n: usize) -> Envelope {
        Envelope {
            abi: ABI.into(),
            attempt: id(n),
            request: Request::AutomaticApprovals {
                request_id: id(100 + n),
                enabled: true,
                categories: None,
            },
        }
    }

    #[test]
    fn stale_native_focus_queries_refuse_without_releasing_original_slot_resources() {
        use crate::desktop::{
            notification_control::{Command as Control, FocusState, Scope},
            ownership::Mode,
            Build,
        };
        let responder = Responder::new(
            "com.hgdev.hgs-tray".into(),
            Build {
                version: "1.2.3".into(),
                source_commit: "a".repeat(40),
                source_dirty: Some(false),
            },
            id(80),
            true,
            false,
        );
        assert!(responder.configure_once("a".repeat(64)));
        assert!(responder.configure_ownership_mode_once(Mode::Service));
        let scope = Scope {
            configuration: "a".repeat(64),
            service_epoch: id(81),
        };
        let connection = responder.notification_connect(scope.clone()).unwrap();
        connection.observe(FocusState::Foreground).unwrap();
        let mut held = Vec::new();
        for n in 0..LIMIT {
            let envelope = Envelope {
                abi: ABI.into(),
                attempt: id(n),
                request: Request::NotificationControl {
                    scope: scope.clone(),
                    control: Control::Focus {
                        request_id: id(100 + n),
                    },
                },
            };
            let native = responder.notification_focus.evidence(&scope, true).unwrap();
            let slot = responder
                .ui_requests
                .prepare_with_native(envelope, None, Some(native))
                .unwrap();
            *slot.0.child.lock().unwrap() = Some((23, "synthetic-birth".into()));
            assert_eq!(
                slot.notification_evidence().unwrap().focus,
                connection.snapshot()
            );
            assert!(responder.ui_reply(&id(n)).is_some());
            held.push(slot.resources());
        }
        connection.observe(FocusState::Background).unwrap();
        for n in 0..LIMIT {
            assert!(responder.ui_reply(&id(n)).is_none());
        }
        let envelope = Envelope {
            abi: ABI.into(),
            attempt: id(99),
            request: Request::NotificationControl {
                scope: scope.clone(),
                control: Control::Focus {
                    request_id: id(199),
                },
            },
        };
        let prepare = || {
            responder.ui_requests.prepare_with_native(
                envelope.clone(),
                None,
                responder.notification_focus.evidence(&scope, true),
            )
        };
        assert_eq!(prepare().err().unwrap().kind(), io::ErrorKind::WouldBlock);
        held.clear();
        let slot = prepare().unwrap();
        *slot.0.child.lock().unwrap() = Some((24, "synthetic-birth".into()));
        assert!(responder.ui_reply(&id(99)).is_some());
        connection.invalidate();
        assert!(responder.ui_reply(&id(99)).is_none());
    }

    #[test]
    fn termination_retention_requires_cached_service_mode_and_exact_configuration() {
        use crate::desktop::{
            ownership::Mode,
            termination_outcome::{Choice, Intent, Scope},
        };
        let capture = crate::termination_cleanup::Capture {
            target: crate::Target {
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
            String::from_utf8(capture.invocation(&id(101)).unwrap().stdin.unwrap()).unwrap();
        let intent = Intent {
            scope: Scope {
                configuration: "a".repeat(64),
                service_epoch: id(4),
            },
            confirmation_id: id(102),
            choice: Choice::RemoveAfterTerminate,
            request_id: id(101),
            capture,
            original,
        };
        let mut envelope = envelope(1);
        envelope.request = Request::TerminationOutcome {
            intent: Box::new(intent),
        };
        let cache = Cache::default();
        assert!(cache.prepare(envelope.clone()).is_err());
        assert!(cache
            .prepare_with_mode(envelope.clone(), Some(Mode::Native))
            .is_err());
        let slot = cache
            .prepare_with_mode(envelope.clone(), Some(Mode::Service))
            .unwrap();
        *slot.0.child.lock().unwrap() = Some((24, "synthetic-birth".into()));
        assert_eq!(
            cache.evidence(&id(1)).unwrap().ownership_mode,
            Some(Mode::Service)
        );
        let responder = Responder::new(
            "com.hgdev.hgs-tray".into(),
            super::super::Build {
                version: "1.0.0".into(),
                source_commit: "a".repeat(40),
                source_dirty: Some(false),
            },
            id(8),
            true,
            false,
        );
        assert!(responder.configure_ownership_mode_once(Mode::Service));
        assert!(responder.configure_once("b".repeat(64)));
        assert!(responder
            .prepare_ui_request(envelope.attempt.clone(), envelope.request.clone())
            .is_err());
        let decoded: Result<Envelope, _> =
            serde_json::from_value(serde_json::to_value(&envelope).unwrap());
        assert!(decoded.unwrap().valid());
    }

    #[test]
    fn cache_requires_the_original_native_service_mode() {
        use crate::desktop::{
            cache_maintenance::{Command, Timing},
            ownership::Mode,
        };
        let cache = Cache::default();
        let mut value = envelope(1);
        value.request = Request::CacheMaintenance {
            request_id: id(101),
            command: Box::new(Command::Prepare {
                operation_id: id(102),
                configuration: "a".repeat(64),
                root: "/srv/example/cache".into(),
                limit: 1,
                revision: 0,
                timing: Timing::Immediate,
                policy_change: true,
            }),
        };
        assert!(cache.prepare(value.clone()).is_err());
        assert!(cache
            .prepare_with_mode(value.clone(), Some(Mode::Native))
            .is_err());
        let slot = cache.prepare_with_mode(value, Some(Mode::Service)).unwrap();
        *slot.0.child.lock().unwrap() = Some((24, "synthetic-birth".into()));
        assert_eq!(
            cache.evidence(&id(1)).unwrap().ownership_mode,
            Some(Mode::Service)
        );
    }

    #[test]
    fn ownership_mode_is_bound_to_the_native_slot_and_ordinary_evidence_stays_compatible() {
        use crate::desktop::ownership::{Action, Mode};
        let cache = Cache::default();
        let mut value = envelope(1);
        value.request = Request::DesktopOwnership {
            request_id: id(101),
            action: Action::Grant,
            journal: None,
            revision: 0,
        };
        assert!(cache.prepare(value.clone()).is_err());
        assert!(cache
            .prepare_with_mode(value.clone(), Some(Mode::Native))
            .is_err());
        let slot = cache.prepare_with_mode(value, Some(Mode::Service)).unwrap();
        *slot.0.child.lock().unwrap() = Some((23, "synthetic-birth".into()));
        let evidence = cache.evidence(&id(1)).unwrap();
        assert_eq!(evidence.ownership_mode, Some(Mode::Service));
        assert_eq!(evidence.fingerprint, slot.envelope().request.fingerprint());
        let ordinary = cache.prepare(envelope(2)).unwrap();
        *ordinary.0.child.lock().unwrap() = Some((24, "synthetic-birth".into()));
        let value = serde_json::to_value(cache.evidence(&id(2)).unwrap()).unwrap();
        assert!(value.get("ownership_mode").is_none());
    }

    #[test]
    fn creation_evidence_requires_cached_service_mode_and_exact_native_checkpoint() {
        use crate::desktop::ownership::Mode;
        let cache = Cache::default();
        let mut value = envelope(1);
        value.request = Request::CreationClone {
            intent: Box::new(crate::desktop::creation_clone::test_intent()),
        };
        assert!(cache.prepare(value.clone()).is_err());
        assert!(cache
            .prepare_with_mode(value.clone(), Some(Mode::Native))
            .is_err());
        let slot = cache.prepare_with_mode(value, Some(Mode::Service)).unwrap();
        *slot.0.child.lock().unwrap() = Some((23, "synthetic-birth".into()));
        let evidence = cache.evidence(&id(1)).unwrap();
        assert_eq!(evidence.ownership_mode, Some(Mode::Service));
        assert_eq!(evidence.request_id, slot.envelope().request.id());
        assert_eq!(evidence.fingerprint, slot.envelope().request.fingerprint());
        for field in ["mode", "confirmed", "role", "execution_consent"] {
            let mut request = serde_json::to_value(&slot.envelope().request).unwrap();
            request[field] = true.into();
            assert!(serde_json::from_value::<Request>(request).is_err());
        }
    }

    #[test]
    fn worktree_evidence_requires_cached_service_mode_and_the_same_configuration() {
        use crate::desktop::ownership::Mode;
        let intent = crate::desktop::creation_worktree::test_intent();
        let cache = Cache::default();
        let mut value = envelope(1);
        value.request = Request::CreationWorktree {
            intent: Box::new(intent.clone()),
        };
        assert_eq!(value.request.operation(), "creation_worktree");
        assert_eq!(value.request.id(), intent.worktree_request_id);
        assert!(cache.prepare(value.clone()).is_err());
        assert!(cache
            .prepare_with_mode(value.clone(), Some(Mode::Native))
            .is_err());
        let slot = cache
            .prepare_with_mode(value.clone(), Some(Mode::Service))
            .unwrap();
        *slot.0.child.lock().unwrap() = Some((25, "synthetic-birth".into()));
        let evidence = cache.evidence(&id(1)).unwrap();
        assert_eq!(evidence.ownership_mode, Some(Mode::Service));
        assert_eq!(evidence.request_id, intent.worktree_request_id);
        // A clone-shaped body never deserializes as this kind, and no
        // authority claim can ride along.
        let mut request = serde_json::to_value(&value.request).unwrap();
        request["op"] = "creation_clone".into();
        assert!(serde_json::from_value::<Request>(request).is_err());
        for field in ["mode", "confirmed", "role", "execution_consent"] {
            let mut request = serde_json::to_value(&value.request).unwrap();
            request[field] = true.into();
            assert!(serde_json::from_value::<Request>(request).is_err());
        }
    }

    #[test]
    fn launch_evidence_requires_cached_service_mode_and_the_same_configuration() {
        use crate::desktop::ownership::Mode;
        let intent = crate::desktop::creation_launch::test_intent();
        let cache = Cache::default();
        let mut value = envelope(1);
        value.request = Request::CreationLaunch {
            intent: Box::new(intent.clone()),
        };
        assert_eq!(value.request.operation(), "creation_launch");
        assert_eq!(value.request.id(), intent.launch_id);
        assert!(cache.prepare(value.clone()).is_err());
        assert!(cache
            .prepare_with_mode(value.clone(), Some(Mode::Native))
            .is_err());
        let slot = cache
            .prepare_with_mode(value.clone(), Some(Mode::Service))
            .unwrap();
        *slot.0.child.lock().unwrap() = Some((25, "synthetic-birth".into()));
        let evidence = cache.evidence(&id(1)).unwrap();
        assert_eq!(evidence.ownership_mode, Some(Mode::Service));
        assert_eq!(evidence.request_id, intent.launch_id);
        // A clone-shaped body never deserializes as this kind, and no
        // authority claim can ride along.
        let mut request = serde_json::to_value(&value.request).unwrap();
        request["op"] = "creation_worktree".into();
        assert!(serde_json::from_value::<Request>(request).is_err());
        for field in ["mode", "confirmed", "role", "execution_consent"] {
            let mut request = serde_json::to_value(&value.request).unwrap();
            request[field] = true.into();
            assert!(serde_json::from_value::<Request>(request).is_err());
        }
    }

    #[test]
    fn task_start_reconcile_keeps_its_original_body_and_cached_service_mode() {
        use crate::desktop::ownership::Mode;
        let intent = crate::desktop::task_start_reconcile::test_intent();
        let value = Envelope {
            abi: ABI.into(),
            attempt: id(1),
            request: Request::TaskStartReconcile {
                intent: Box::new(intent.clone()),
            },
        };
        assert!(value.valid());
        assert_eq!(value.request.operation(), "task_start_reconcile");
        assert_eq!(value.request.id(), intent.reconcile_id);
        assert_ne!(value.request.id(), intent.binding.key.request_id);
        let cache = Cache::default();
        assert!(cache.prepare(value.clone()).is_err());
        assert!(cache
            .prepare_with_mode(value.clone(), Some(Mode::Native))
            .is_err());
        let slot = cache
            .prepare_with_mode(value.clone(), Some(Mode::Service))
            .unwrap();
        *slot.0.child.lock().unwrap() = Some((26, "synthetic-birth".into()));
        let evidence = cache.evidence(&id(1)).unwrap();
        assert_eq!(evidence.ownership_mode, Some(Mode::Service));
        assert_eq!(evidence.fingerprint, value.request.fingerprint());
        assert!(slot.envelope().request == value.request);
        for field in ["role", "confirmed", "execution_consent", "path"] {
            let mut request = serde_json::to_value(&value.request).unwrap();
            request[field] = true.into();
            assert!(serde_json::from_value::<Request>(request).is_err());
        }
        let mut request = serde_json::to_value(value.request).unwrap();
        request["op"] = "creation_worktree".into();
        assert!(serde_json::from_value::<Request>(request).is_err());
    }

    #[test]
    fn task_message_responder_requires_cached_service_mode_and_the_same_configuration() {
        const CASE: &str = "desktop::ui_requests::tests::task_message_responder_requires_cached_service_mode_and_the_same_configuration";
        if std::env::var_os("ZERUS_TASK_MESSAGE_RESPONDER_TEST_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", CASE, "--test-threads=1", "--nocapture"])
                .env_remove("HGS_SESSION")
                .env_remove("HGS_SESSION_ID")
                .env_remove("HGS_RUN_ID")
                .env("ZERUS_TASK_MESSAGE_RESPONDER_TEST_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
            return;
        }
        use crate::desktop::{ownership::Mode, Build};
        let intent = crate::desktop::task_message::test_intent();
        let request = Request::TaskMessage {
            intent: Box::new(intent.clone()),
        };
        assert_eq!(request.operation(), "task_message");
        assert_eq!(request.id(), intent.request_id);
        // The slot itself binds only the Service mode.
        let cache = Cache::default();
        let value = Envelope {
            abi: ABI.into(),
            attempt: id(1),
            request: request.clone(),
        };
        assert!(cache.prepare(value.clone()).is_err());
        assert!(cache
            .prepare_with_mode(value.clone(), Some(Mode::Native))
            .is_err());
        // The responder admits only its own cached Service mode and exactly
        // the configuration the original was scoped to.
        for (configuration, mode, accepted) in [
            (intent.scope.configuration.clone(), None, false),
            (
                intent.scope.configuration.clone(),
                Some(Mode::Native),
                false,
            ),
            ("f".repeat(64), Some(Mode::Service), false),
            (
                intent.scope.configuration.clone(),
                Some(Mode::Service),
                true,
            ),
        ] {
            let responder = Responder::new(
                "com.hgdev.hgs-tray".into(),
                Build {
                    version: "1.2.3".into(),
                    source_commit: "a".repeat(40),
                    source_dirty: Some(false),
                },
                id(80),
                true,
                false,
            );
            assert!(responder.configure_once(configuration));
            if let Some(mode) = mode {
                assert!(responder.configure_ownership_mode_once(mode));
            }
            let prepared = responder.prepare_ui_request(id(1), request.clone());
            assert_eq!(prepared.is_ok(), accepted);
            if let Ok(slot) = prepared {
                *slot.0.child.lock().unwrap() = Some((27, "synthetic-birth".into()));
                let evidence = responder.ui_requests.evidence(&id(1)).unwrap();
                assert_eq!(evidence.ownership_mode, Some(Mode::Service));
                assert_eq!(evidence.request_id, intent.request_id);
                assert_eq!(evidence.fingerprint, request.fingerprint());
            }
        }
    }

    #[test]
    fn task_message_input_release_responder_requires_cached_service_mode_and_the_same_configuration(
    ) {
        const CASE: &str = "desktop::ui_requests::tests::task_message_input_release_responder_requires_cached_service_mode_and_the_same_configuration";
        if std::env::var_os("ZERUS_TASK_MESSAGE_INPUT_RELEASE_RESPONDER_TEST_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", CASE, "--test-threads=1", "--nocapture"])
                .env_remove("HGS_SESSION")
                .env_remove("HGS_SESSION_ID")
                .env_remove("HGS_RUN_ID")
                .env("ZERUS_TASK_MESSAGE_INPUT_RELEASE_RESPONDER_TEST_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
            return;
        }
        use crate::desktop::{ownership::Mode, Build};
        let release = crate::desktop::task_message::review::tests::release();
        let request = Request::TaskMessageInputRelease {
            release: Box::new(release.clone()),
        };
        assert_eq!(request.operation(), "task_message_input_release");
        assert_eq!(request.id(), release.release_id);
        // An invalid release never prepares.
        let invalid = Request::TaskMessageInputRelease {
            release: Box::new(crate::desktop::task_message::review::Release {
                bytes: 0,
                ..release.clone()
            }),
        };
        // The responder admits only its own cached Service mode and exactly
        // the configuration the release was scoped to.
        for (configuration, mode, request, accepted) in [
            (release.scope.configuration.clone(), None, &request, false),
            (
                release.scope.configuration.clone(),
                Some(Mode::Native),
                &request,
                false,
            ),
            ("f".repeat(64), Some(Mode::Service), &request, false),
            (
                release.scope.configuration.clone(),
                Some(Mode::Service),
                &invalid,
                false,
            ),
            (
                release.scope.configuration.clone(),
                Some(Mode::Service),
                &request,
                true,
            ),
        ] {
            let responder = Responder::new(
                "com.hgdev.hgs-tray".into(),
                Build {
                    version: "1.2.3".into(),
                    source_commit: "a".repeat(40),
                    source_dirty: Some(false),
                },
                id(80),
                true,
                false,
            );
            assert!(responder.configure_once(configuration));
            if let Some(mode) = mode {
                assert!(responder.configure_ownership_mode_once(mode));
            }
            let prepared = responder.prepare_ui_request(id(1), request.clone());
            assert_eq!(prepared.is_ok(), accepted);
            if let Ok(slot) = prepared {
                *slot.0.child.lock().unwrap() = Some((27, "synthetic-birth".into()));
                let evidence = responder.ui_requests.evidence(&id(1)).unwrap();
                assert_eq!(evidence.ownership_mode, Some(Mode::Service));
                assert_eq!(evidence.request_id, release.release_id);
                assert_eq!(evidence.fingerprint, request.fingerprint());
            }
        }
    }

    #[test]
    fn deferred_send_responder_requires_cached_service_mode_and_the_same_configuration() {
        const CASE: &str = "desktop::ui_requests::tests::deferred_send_responder_requires_cached_service_mode_and_the_same_configuration";
        if std::env::var_os("ZERUS_DEFERRED_SEND_RESPONDER_TEST_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", CASE, "--test-threads=1", "--nocapture"])
                .env_remove("HGS_SESSION")
                .env_remove("HGS_SESSION_ID")
                .env_remove("HGS_RUN_ID")
                .env("ZERUS_DEFERRED_SEND_RESPONDER_TEST_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
            return;
        }
        use crate::desktop::{ownership::Mode, Build};
        let intent = crate::desktop::deferred_send::tests::intent();
        let request = Request::DeferredSend {
            intent: Box::new(intent.clone()),
        };
        assert_eq!(request.operation(), "deferred_send");
        assert_eq!(request.id(), intent.request_id);
        // An intent that does not verify never prepares.
        let invalid = Request::DeferredSend {
            intent: Box::new(crate::desktop::deferred_send::Intent {
                original: "{}".into(),
                ..intent.clone()
            }),
        };
        // The responder admits only its own cached Service mode and exactly
        // the configuration the intent was scoped to.
        for (configuration, mode, request, accepted) in [
            (intent.scope.configuration.clone(), None, &request, false),
            (
                intent.scope.configuration.clone(),
                Some(Mode::Native),
                &request,
                false,
            ),
            ("f".repeat(64), Some(Mode::Service), &request, false),
            (
                intent.scope.configuration.clone(),
                Some(Mode::Service),
                &invalid,
                false,
            ),
            (
                intent.scope.configuration.clone(),
                Some(Mode::Service),
                &request,
                true,
            ),
        ] {
            let responder = Responder::new(
                "com.hgdev.hgs-tray".into(),
                Build {
                    version: "1.2.3".into(),
                    source_commit: "a".repeat(40),
                    source_dirty: Some(false),
                },
                id(80),
                true,
                false,
            );
            assert!(responder.configure_once(configuration));
            if let Some(mode) = mode {
                assert!(responder.configure_ownership_mode_once(mode));
            }
            let prepared = responder.prepare_ui_request(id(1), request.clone());
            assert_eq!(prepared.is_ok(), accepted);
            if let Ok(slot) = prepared {
                *slot.0.child.lock().unwrap() = Some((27, "synthetic-birth".into()));
                let evidence = responder.ui_requests.evidence(&id(1)).unwrap();
                assert_eq!(evidence.ownership_mode, Some(Mode::Service));
                assert_eq!(evidence.request_id, intent.request_id);
                assert_eq!(evidence.fingerprint, request.fingerprint());
            }
        }
    }

    #[test]
    fn deferred_answer_responder_requires_cached_service_mode_and_the_same_configuration() {
        const CASE: &str = "desktop::ui_requests::tests::deferred_answer_responder_requires_cached_service_mode_and_the_same_configuration";
        if std::env::var_os("ZERUS_DEFERRED_ANSWER_RESPONDER_TEST_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", CASE, "--test-threads=1", "--nocapture"])
                .env_remove("HGS_SESSION")
                .env_remove("HGS_SESSION_ID")
                .env_remove("HGS_RUN_ID")
                .env("ZERUS_DEFERRED_ANSWER_RESPONDER_TEST_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
            return;
        }
        use crate::desktop::{ownership::Mode, Build};
        let intent = crate::desktop::deferred_answer::tests::intent();
        let request = Request::DeferredAnswer {
            intent: Box::new(intent.clone()),
        };
        assert_eq!(request.operation(), "deferred_answer");
        assert_eq!(request.id(), intent.request_id);
        // An intent that does not verify never prepares.
        let invalid = Request::DeferredAnswer {
            intent: Box::new(crate::desktop::deferred_answer::Intent {
                original: "{}".into(),
                ..intent.clone()
            }),
        };
        // The responder admits only its own cached Service mode and exactly
        // the configuration the intent was scoped to.
        for (configuration, mode, request, accepted) in [
            (intent.scope.configuration.clone(), None, &request, false),
            (
                intent.scope.configuration.clone(),
                Some(Mode::Native),
                &request,
                false,
            ),
            ("f".repeat(64), Some(Mode::Service), &request, false),
            (
                intent.scope.configuration.clone(),
                Some(Mode::Service),
                &invalid,
                false,
            ),
            (
                intent.scope.configuration.clone(),
                Some(Mode::Service),
                &request,
                true,
            ),
        ] {
            let responder = Responder::new(
                "com.hgdev.hgs-tray".into(),
                Build {
                    version: "1.2.3".into(),
                    source_commit: "a".repeat(40),
                    source_dirty: Some(false),
                },
                id(80),
                true,
                false,
            );
            assert!(responder.configure_once(configuration));
            if let Some(mode) = mode {
                assert!(responder.configure_ownership_mode_once(mode));
            }
            let prepared = responder.prepare_ui_request(id(1), request.clone());
            assert_eq!(prepared.is_ok(), accepted);
            if let Ok(slot) = prepared {
                *slot.0.child.lock().unwrap() = Some((27, "synthetic-birth".into()));
                let evidence = responder.ui_requests.evidence(&id(1)).unwrap();
                assert_eq!(evidence.ownership_mode, Some(Mode::Service));
                assert_eq!(evidence.request_id, intent.request_id);
                assert_eq!(evidence.fingerprint, request.fingerprint());
            }
        }
    }

    #[test]
    fn compact_to_send_responder_requires_cached_service_mode_and_the_same_configuration() {
        const CASE: &str = "desktop::ui_requests::tests::compact_to_send_responder_requires_cached_service_mode_and_the_same_configuration";
        if std::env::var_os("ZERUS_COMPACT_TO_SEND_RESPONDER_TEST_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", CASE, "--test-threads=1", "--nocapture"])
                .env_remove("HGS_SESSION")
                .env_remove("HGS_SESSION_ID")
                .env_remove("HGS_RUN_ID")
                .env("ZERUS_COMPACT_TO_SEND_RESPONDER_TEST_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
            return;
        }
        use crate::desktop::{ownership::Mode, Build};
        let intent = crate::desktop::compact_to_send::tests::intent();
        let request = Request::CompactToSend {
            intent: Box::new(intent.clone()),
        };
        assert_eq!(request.operation(), "compact_to_send");
        assert_eq!(request.id(), intent.request_id);
        // An intent that does not verify never prepares.
        let invalid = Request::CompactToSend {
            intent: Box::new(crate::desktop::compact_to_send::Intent {
                original: "{}".into(),
                ..intent.clone()
            }),
        };
        // The responder admits only its own cached Service mode and exactly
        // the configuration the intent was scoped to.
        for (configuration, mode, request, accepted) in [
            (intent.scope.configuration.clone(), None, &request, false),
            (
                intent.scope.configuration.clone(),
                Some(Mode::Native),
                &request,
                false,
            ),
            ("f".repeat(64), Some(Mode::Service), &request, false),
            (
                intent.scope.configuration.clone(),
                Some(Mode::Service),
                &invalid,
                false,
            ),
            (
                intent.scope.configuration.clone(),
                Some(Mode::Service),
                &request,
                true,
            ),
        ] {
            let responder = Responder::new(
                "com.hgdev.hgs-tray".into(),
                Build {
                    version: "1.2.3".into(),
                    source_commit: "a".repeat(40),
                    source_dirty: Some(false),
                },
                id(80),
                true,
                false,
            );
            assert!(responder.configure_once(configuration));
            if let Some(mode) = mode {
                assert!(responder.configure_ownership_mode_once(mode));
            }
            let prepared = responder.prepare_ui_request(id(1), request.clone());
            assert_eq!(prepared.is_ok(), accepted);
            if let Ok(slot) = prepared {
                *slot.0.child.lock().unwrap() = Some((27, "synthetic-birth".into()));
                let evidence = responder.ui_requests.evidence(&id(1)).unwrap();
                assert_eq!(evidence.ownership_mode, Some(Mode::Service));
                assert_eq!(evidence.request_id, intent.request_id);
                assert_eq!(evidence.fingerprint, request.fingerprint());
            }
        }
    }

    #[test]
    fn session_resume_responder_requires_cached_service_mode_and_the_same_configuration() {
        const CASE: &str = "desktop::ui_requests::tests::session_resume_responder_requires_cached_service_mode_and_the_same_configuration";
        if std::env::var_os("ZERUS_SESSION_RESUME_RESPONDER_TEST_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", CASE, "--test-threads=1", "--nocapture"])
                .env_remove("HGS_SESSION")
                .env_remove("HGS_SESSION_ID")
                .env_remove("HGS_RUN_ID")
                .env("ZERUS_SESSION_RESUME_RESPONDER_TEST_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
            return;
        }
        use crate::desktop::{ownership::Mode, Build};
        let intent = crate::desktop::session_resume::tests::intent();
        let request = Request::SessionResume {
            intent: Box::new(intent.clone()),
        };
        assert_eq!(request.operation(), "session_resume");
        assert_eq!(request.id(), intent.request_id);
        // An intent that does not verify never prepares.
        let invalid = Request::SessionResume {
            intent: Box::new(crate::desktop::session_resume::Intent {
                original: "{}".into(),
                ..intent.clone()
            }),
        };
        // The responder admits only its own cached Service mode and exactly
        // the configuration the intent was scoped to.
        for (configuration, mode, request, accepted) in [
            (intent.scope.configuration.clone(), None, &request, false),
            (
                intent.scope.configuration.clone(),
                Some(Mode::Native),
                &request,
                false,
            ),
            ("f".repeat(64), Some(Mode::Service), &request, false),
            (
                intent.scope.configuration.clone(),
                Some(Mode::Service),
                &invalid,
                false,
            ),
            (
                intent.scope.configuration.clone(),
                Some(Mode::Service),
                &request,
                true,
            ),
        ] {
            let responder = Responder::new(
                "com.hgdev.hgs-tray".into(),
                Build {
                    version: "1.2.3".into(),
                    source_commit: "a".repeat(40),
                    source_dirty: Some(false),
                },
                id(80),
                true,
                false,
            );
            assert!(responder.configure_once(configuration));
            if let Some(mode) = mode {
                assert!(responder.configure_ownership_mode_once(mode));
            }
            let prepared = responder.prepare_ui_request(id(1), request.clone());
            assert_eq!(prepared.is_ok(), accepted);
            if let Ok(slot) = prepared {
                *slot.0.child.lock().unwrap() = Some((27, "synthetic-birth".into()));
                let evidence = responder.ui_requests.evidence(&id(1)).unwrap();
                assert_eq!(evidence.ownership_mode, Some(Mode::Service));
                assert_eq!(evidence.request_id, intent.request_id);
                assert_eq!(evidence.fingerprint, request.fingerprint());
            }
        }
    }

    #[test]
    fn task_start_native_responder_uses_its_cached_configuration_and_mode() {
        const CASE: &str = "desktop::ui_requests::tests::task_start_native_responder_uses_its_cached_configuration_and_mode";
        if std::env::var_os("ZERUS_TASK_START_RESPONDER_TEST_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", CASE, "--test-threads=1", "--nocapture"])
                .env_remove("HGS_SESSION")
                .env_remove("HGS_SESSION_ID")
                .env_remove("HGS_RUN_ID")
                .env("ZERUS_TASK_START_RESPONDER_TEST_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
            return;
        }
        use crate::desktop::{ownership::Mode, Build};
        let intent = crate::desktop::task_start_reconcile::test_intent();
        for (configuration, mode, accepted) in [
            (intent.scope.configuration.clone(), None, false),
            (
                intent.scope.configuration.clone(),
                Some(Mode::Native),
                false,
            ),
            ("f".repeat(64), Some(Mode::Service), false),
            (
                intent.scope.configuration.clone(),
                Some(Mode::Service),
                true,
            ),
        ] {
            let responder = Responder::new(
                "com.hgdev.hgs-tray".into(),
                Build {
                    version: "1.2.3".into(),
                    source_commit: "a".repeat(40),
                    source_dirty: Some(false),
                },
                id(80),
                true,
                false,
            );
            assert!(responder.configure_once(configuration));
            if let Some(mode) = mode {
                assert!(responder.configure_ownership_mode_once(mode));
            }
            let prepared = responder.prepare_ui_request(
                id(1),
                Request::TaskStartReconcile {
                    intent: Box::new(intent.clone()),
                },
            );
            assert_eq!(prepared.is_ok(), accepted);
        }
    }

    #[test]
    fn fixed_query_and_request_shapes_do_not_accept_authority_claims() {
        let value = envelope(1);
        assert_eq!(query(&value.attempt).unwrap().len(), 59);
        assert_eq!(
            query_attempt(query(&value.attempt).unwrap().strip_suffix(b"\n").unwrap()),
            Some(value.attempt.as_str())
        );
        assert!(query("unknown").is_none());
        for field in ["confirmed", "role", "caller_pid", "token"] {
            let mut bad = serde_json::to_value(&value).unwrap();
            bad[field] = true.into();
            assert!(serde_json::from_value::<Envelope>(bad).is_err());
        }
        let mut bad = serde_json::to_value(&value.request).unwrap();
        bad["confirmed"] = true.into();
        assert!(serde_json::from_value::<Request>(bad).is_err());
    }

    #[test]
    fn maintenance_configure_requires_both_subsystems_and_uses_the_same_fingerprint() {
        let value = serde_json::json!({"op":"maintenance_configure","request_id":id(4),
            "update":null,"recovery":null});
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        let envelope = Envelope {
            abi: ABI.into(),
            attempt: id(5),
            request,
        };
        assert!(envelope.valid());
        assert_eq!(serde_json::to_value(&envelope.request).unwrap(), value);
        assert_eq!(envelope.request.operation(), "maintenance_configure");
        for field in ["update", "recovery"] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Request>(missing).is_err());
        }
        let mut claimed = value;
        claimed["confirmed"] = true.into();
        assert!(serde_json::from_value::<Request>(claimed).is_err());
        assert_ne!(
            envelope.request.fingerprint(),
            Request::AutomaticApprovals {
                request_id: id(4),
                enabled: false,
                categories: None,
            }
            .fingerprint()
        );
    }

    #[test]
    fn unbound_expired_or_exhausted_slots_cannot_supply_evidence_or_recycle_resources() {
        let cache = Cache::default();
        let mut held = Vec::new();
        for n in 0..LIMIT {
            held.push(cache.prepare(envelope(n)).unwrap());
        }
        assert!(
            cache.evidence(&id(0)).is_none(),
            "a reservation is not an original child"
        );
        assert!(cache
            .evidence_at(&id(0), Instant::now() + LIFETIME)
            .is_none());
        assert!(
            cache.prepare(envelope(LIMIT)).is_err(),
            "expiration must not recycle retained native resource charges"
        );
        drop(held.pop());
        assert!(cache.prepare(envelope(LIMIT)).is_ok());
        assert!(
            cache.prepare(envelope(0)).is_err(),
            "live attempts cannot be overwritten"
        );
    }

    #[test]
    fn original_child_fixture() {
        if std::env::var_os("ZERUS_UI_CHILD_FIXTURE").is_some() {
            let mut bytes = Vec::new();
            std::io::stdin().take(4096).read_to_end(&mut bytes).unwrap();
        }
    }

    #[test]
    fn native_serial_survives_slot_release_and_never_wraps() {
        let cache = Cache::default();
        let first = cache.prepare(envelope(1)).unwrap();
        assert_eq!(first.0.serial, 1);
        drop(first);
        let next = cache.prepare(envelope(2)).unwrap();
        assert_eq!(next.0.serial, 2);
        cache.0.lock().unwrap().serial = u64::MAX;
        assert!(cache.prepare(envelope(3)).is_err());
    }

    #[test]
    fn evidence_binds_an_original_child_and_lasts_with_its_resources() {
        let cache = Cache::default();
        let registration = cache.prepare(envelope(1)).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "desktop::ui_requests::tests::original_child_fixture",
            ])
            .env("ZERUS_UI_CHILD_FIXTURE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        assert!(registration.bind_original(&child));
        assert!(
            !registration.bind_original(&child),
            "the original child binding is immutable"
        );
        let evidence = cache.evidence(&id(1)).unwrap();
        assert_eq!(evidence.child_pid, child.id());
        assert_eq!(evidence.fingerprint, envelope(1).request.fingerprint());
        assert!(!evidence.child_birth.is_empty());
        let resources = registration.resources();
        drop(registration);
        assert!(cache.evidence(&id(1)).is_some());
        drop(child.stdin.take());
        let end = Instant::now() + Duration::from_secs(5);
        let exited = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status.success();
            }
            if Instant::now() >= end {
                child.kill().unwrap();
                child.wait().unwrap();
                break false;
            }
            thread::sleep(Duration::from_millis(1));
        };
        drop(resources);
        assert!(exited);
        assert!(cache.evidence(&id(1)).is_none());
    }
}
