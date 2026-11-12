//! Native, original-child frontend requests. A transport error never permits
//! replay: each operation keeps one logical intent until its receipt settles.
use super::*;
use serde::Deserialize;
use zerus_client_core::{
    desktop::{
        relay,
        ui_requests::{self, Request},
    },
    maintenance,
    run::{self, OwnedError},
    Invocation,
};
const MAINTENANCE_UNAVAILABLE: &str =
    zerus_client_core::source!("Maintenance settings are unavailable from the desktop service.");

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    #[serde(skip)] // Original native Terminate capture only, never renderer JSON.
    TerminationOutcome,
    #[serde(skip)] // Closed native original helper only.
    WorktreeProgramStop,
    #[serde(skip)] // Only the live native Remove continuation; never renderer JSON.
    WorktreeProgramCleanup,
    #[serde(skip)] // Native Files controller only, never caller-chosen paths.
    CacheMaintenance,
    #[serde(skip)] // Native creation owner only; never generic policy dispatch or receipts.
    CreationClone,
    #[serde(skip)] // Native creation owner only; never generic policy dispatch or receipts.
    CreationWorktree,
    #[serde(skip)] // Native creation owner only; never generic policy dispatch or receipts.
    CreationLaunch,
    #[serde(skip)] // Native task message owner only; never generic policy dispatch or receipts.
    TaskMessage,
    #[serde(skip)] // Native input review only; never generic policy dispatch or receipts.
    TaskMessageInputRelease,
    #[serde(skip)] // Native deferred send owner only; never generic policy dispatch or receipts.
    DeferredSend,
    #[serde(skip)] // Native deferred answer owner only; never generic policy dispatch or receipts.
    DeferredAnswer,
    #[serde(skip)] // Native compact-to-send owner only; never generic policy dispatch or receipts.
    CompactToSend,
    #[serde(skip)] // Native session resume owner only; never generic policy dispatch or receipts.
    SessionResume,
    #[serde(skip)] // Native TaskStart boundary only, never generic renderer JSON.
    TaskStartReconcile,
    #[serde(skip)] // Separate native notification controller, never generic policy JSON.
    NotificationControl,
    #[serde(skip)] // Separate native ownership controller, never a generic policy request.
    DesktopOwnership,
    #[serde(skip)] // Closed native observation, never a policy operation or Grant receipt.
    DesktopOwnershipObserve,
    #[serde(skip)] // Fixed native startup producer only, never JS row/path input.
    ImportPendingReads,
    #[serde(skip)] // The ephemeral consumer is separate from durable policy requests.
    ActivationWait,
    AutomaticApprovals,
    QuestionRoutes,
    QuestionRouteChild,
    MaintenanceConfigure,
}
impl Operation {
    fn of(request: &Request) -> Self {
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
            Request::NotificationControl { .. } => Self::NotificationControl,
            Request::DesktopOwnership { .. } => Self::DesktopOwnership,
            Request::DesktopOwnershipObserve { .. } => Self::DesktopOwnershipObserve,
            Request::ImportPendingReads { .. } => Self::ImportPendingReads,
            Request::ActivationWait { .. } => Self::ActivationWait,
            Request::AutomaticApprovals { .. } => Self::AutomaticApprovals,
            Request::QuestionRoutes { .. } => Self::QuestionRoutes,
            Request::QuestionRouteChild { .. } => Self::QuestionRouteChild,
            Request::MaintenanceConfigure { .. } => Self::MaintenanceConfigure,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::CacheMaintenance => "cache_maintenance",
            Self::TerminationOutcome => "termination_outcome",
            Self::WorktreeProgramStop => "worktree_program_stop",
            Self::WorktreeProgramCleanup => "worktree_program_cleanup",
            Self::CreationClone => "creation_clone",
            Self::CreationWorktree => "creation_worktree",
            Self::CreationLaunch => "creation_launch",
            Self::TaskMessage => "task_message",
            Self::TaskMessageInputRelease => "task_message_input_release",
            Self::DeferredSend => "deferred_send",
            Self::DeferredAnswer => "deferred_answer",
            Self::CompactToSend => "compact_to_send",
            Self::SessionResume => "session_resume",
            Self::TaskStartReconcile => "task_start_reconcile",
            Self::NotificationControl => "notification_controls",
            Self::DesktopOwnership => "desktop_ownership",
            Self::DesktopOwnershipObserve => "desktop_ownership_observe",
            Self::ImportPendingReads => "import_pending_reads",
            Self::ActivationWait => "activation_wait",
            Self::AutomaticApprovals => "automatic_approvals",
            Self::QuestionRoutes => "question_routes",
            Self::QuestionRouteChild => "question_route_child",
            Self::MaintenanceConfigure => "maintenance_configure",
        }
    }
}
#[derive(Default)]
pub(super) struct Frontend {
    state: Arc<Mutex<Pending>>,
}
#[derive(Default)]
struct Pending {
    next: u64,
    operations: BTreeMap<Operation, Entry>,
}
struct Entry {
    request: Request,
    token: u64,
    running: bool,
    complete: bool,
    original_exit_confirmed: bool,
    result: Option<Value>,
}
enum Begin {
    Immediate(Value),
    Work(Reservation),
}
struct Reservation {
    state: Arc<Mutex<Pending>>,
    request: Request,
    token: u64,
    lookup: bool,
    original_exit_confirmed: bool,
    armed: bool,
}
enum Receipt {
    Present(Value),
    Absent,
}
impl Frontend {
    fn begin(&self, operation: Operation, requested: Option<Request>) -> Begin {
        if let Some(request @ Request::DesktopOwnershipObserve { .. }) = &requested {
            return Begin::Immediate(refused(request, "native_observation_required"));
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(old) = state.operations.get_mut(&operation) {
            if let Some(request) = &requested {
                if request.id() == old.request.id() && request != &old.request {
                    return Begin::Immediate(refused(request, "frontend_request_identity_changed"));
                }
                if request.id() != old.request.id() && !old.complete {
                    return Begin::Immediate(refused(
                        request,
                        "previous_frontend_request_unconfirmed",
                    ));
                }
            }
            let same = requested
                .as_ref()
                .is_none_or(|r| r.id() == old.request.id());
            if same {
                if old.complete {
                    return Begin::Immediate(old.result.clone().expect("completed receipt"));
                }
                if old.running {
                    return Begin::Immediate(unknown(&old.request, "frontend_request_busy"));
                }
                old.running = true;
                return Begin::Work(Reservation {
                    state: self.state.clone(),
                    request: old.request.clone(),
                    token: old.token,
                    lookup: true,
                    original_exit_confirmed: old.original_exit_confirmed,
                    armed: true,
                });
            }
        }
        let Some(request) = requested else {
            return Begin::Immediate(json!({"op":operation.name(),"pending":false}));
        };
        let Some(token) = state.next.checked_add(1) else {
            return Begin::Immediate(refused(&request, "frontend_request_capacity"));
        };
        state.next = token;
        state.operations.insert(
            operation,
            Entry {
                request: request.clone(),
                token,
                running: true,
                complete: false,
                original_exit_confirmed: false,
                result: None,
            },
        );
        Begin::Work(Reservation {
            state: self.state.clone(),
            request,
            token,
            lookup: false,
            original_exit_confirmed: false,
            armed: true,
        })
    }
    fn view(&self, operation: Operation) -> Value {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.operations.get(&operation).map_or_else(
            || json!({"op":operation.name(),"pending":false}),
            |entry| {
                if entry.complete {
                    entry.result.clone().expect("completed receipt")
                } else {
                    unknown(&entry.request, "frontend_request_busy")
                }
            },
        )
    }
}
impl Reservation {
    fn record_original_exit(&mut self, result: &Result<Vec<u8>, OwnedError>) {
        // Called only after the original spawn hook returned a Child. All
        // runner outcomes except ExitUnverified confirm that child's exit,
        // including failures with output pipes still held by a descendant.
        self.original_exit_confirmed = !matches!(result, Err(OwnedError::ExitUnverified));
    }
    fn finish(mut self, value: Value, complete: bool) -> Value {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = state
            .operations
            .get_mut(&Operation::of(&self.request))
            .filter(|e| e.token == self.token)
        {
            entry.running = false;
            entry.complete = complete;
            entry.original_exit_confirmed |= self.original_exit_confirmed;
            entry.result = Some(value.clone());
        }
        self.armed = false;
        value
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = state
            .operations
            .get_mut(&Operation::of(&self.request))
            .filter(|e| e.token == self.token)
        {
            entry.running = false;
            entry.complete = false;
            entry.original_exit_confirmed |= self.original_exit_confirmed;
            entry.result = Some(unknown(&self.request, "frontend_request_unconfirmed"));
        }
    }
}
fn refused(request: &Request, code: &str) -> Value {
    let mut value = json!({"request_id":request.id(),"op":request.operation(),"accepted":false,"durable":false,"pending":false,"code":code});
    recovery_edit(request, &mut value);
    value
}
fn recovery_edit(request: &Request, value: &mut Value) {
    if let Request::MaintenanceConfigure { recovery, .. } = request {
        value["recovery_edit"] = json!(recovery.as_ref().is_some_and(|c| c.policy.is_some()));
    }
}
fn unknown(request: &Request, code: &str) -> Value {
    let mut result = json!({"request_id":request.id(),"op":request.operation(),"accepted":null,"durable":false,"pending":true,"code":code});
    if let Request::AutomaticApprovals {
        enabled,
        categories,
        ..
    } = request
    {
        result["enabled"] = json!(enabled);
        if let Some(categories) = categories {
            result["categories"] = json!(categories);
        }
    }
    if let Request::QuestionRoutes { enabled, .. } = request {
        result["enabled"] = json!(enabled);
    }
    recovery_edit(request, &mut result);
    result
}
fn canonical(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|v| v.to_string() == id)
}

/// Fill this from the running app, never from JS or the selected CLI's build.
fn native_gui(bundle_id: &str) -> Result<maintenance::GuiIdentity, &'static str> {
    let executable = std::env::current_exe().map_err(|_| "frontend_gui_unavailable")?;
    let build = crate::cli::build_info(&crate::cli::BUILD);
    let gui = maintenance::GuiIdentity {
        bundle_id: bundle_id.into(),
        executable: executable
            .to_str()
            .ok_or("frontend_gui_unavailable")?
            .into(),
        app_version: build["versionName"]
            .as_str()
            .ok_or("frontend_gui_unavailable")?
            .into(),
        dev_number: version_dev_number(
            build["versionName"]
                .as_str()
                .ok_or("frontend_gui_unavailable")?,
        )
        .ok_or("frontend_gui_unavailable")?,
        source_commit: build["sourceCommit"]
            .as_str()
            .ok_or("frontend_gui_unavailable")?
            .into(),
    };
    gui.valid().then_some(gui).ok_or("frontend_gui_unavailable")
}
fn version_dev_number(version: &str) -> Option<u64> {
    match version.split_once('-').map(|(_, prerelease)| prerelease) {
        Some(prerelease) if prerelease.starts_with("dev.") => prerelease[4..]
            .parse::<u64>()
            .ok()
            .filter(|number| *number > 0 && number.to_string() == prerelease[4..]),
        _ => Some(0),
    }
}

enum Input {
    Cache {
        id: String,
        command: Box<zerus_client_core::desktop::cache_maintenance::Command>,
    },
    ImportReads {
        id: String,
        page: Box<zerus_client_core::desktop::pending_reads::Page>,
    },
    Approvals {
        id: String,
        enabled: bool,
        categories: Option<Vec<String>>,
    },
    QuestionRoutes {
        id: String,
        enabled: bool,
    },
    QuestionRouteChild(Box<Request>),
    Maintenance {
        id: String,
        configuration: Box<maintenance::Configuration>,
        bundle_id: String,
    },
    Result(Operation),
}
impl Input {
    fn operation(&self) -> Operation {
        match self {
            Self::Cache { .. } => Operation::CacheMaintenance,
            Self::ImportReads { .. } => Operation::ImportPendingReads,
            Self::Approvals { .. } => Operation::AutomaticApprovals,
            Self::QuestionRoutes { .. } => Operation::QuestionRoutes,
            Self::QuestionRouteChild(_) => Operation::QuestionRouteChild,
            Self::Maintenance { .. } => Operation::MaintenanceConfigure,
            Self::Result(op) => *op,
        }
    }
    fn request(self) -> Result<Option<Request>, &'static str> {
        match self {
            Self::Cache { id, command } => {
                if !command.valid(&id) {
                    return Err("invalid_cache_intent");
                }
                Ok(Some(Request::CacheMaintenance {
                    request_id: id,
                    command,
                }))
            }
            Self::ImportReads { id, page } => Ok(Some(Request::ImportPendingReads {
                request_id: id,
                page,
            })),
            Self::Approvals {
                id,
                enabled,
                categories,
            } => Ok(Some(Request::AutomaticApprovals {
                request_id: id,
                enabled,
                categories,
            })),
            Self::QuestionRoutes { id, enabled } => Ok(Some(Request::QuestionRoutes {
                request_id: id,
                enabled,
            })),
            Self::QuestionRouteChild(request) => Ok(Some(*request)),
            Self::Maintenance {
                id,
                mut configuration,
                bundle_id,
            } => {
                if let Some(update) = &mut configuration.update {
                    update.gui = native_gui(&bundle_id)?;
                }
                if !configuration.valid() {
                    return Err("invalid_maintenance_configuration");
                }
                Ok(Some(Request::maintenance_configure(id, *configuration)))
            }
            Self::Result(_) => Ok(None),
        }
    }
    fn refusal(&self, code: &str) -> Value {
        match self {
            Self::Cache { id, .. } => json!({"request_id":id,"op":"cache_maintenance",
                "accepted":false,"durable":false,"pending":false,"code":code}),
            Self::ImportReads { id, .. } => json!({"request_id":id,"op":"import_pending_reads",
                "accepted":false,"durable":false,"pending":false,"code":code}),
            Self::Approvals {
                id,
                enabled,
                categories,
            } => refused(
                &Request::AutomaticApprovals {
                    request_id: id.clone(),
                    enabled: *enabled,
                    categories: categories.clone(),
                },
                code,
            ),
            Self::QuestionRoutes { id, enabled } => {
                json!({"request_id":id,"op":"question_routes","enabled":enabled,"accepted":false,"durable":false,"pending":false,"code":code})
            }
            Self::QuestionRouteChild(request) => refused(request, code),
            Self::Maintenance { id, .. } => {
                json!({"request_id":id,"op":"maintenance_configure","accepted":false,"durable":false,"pending":false,"code":code})
            }
            Self::Result(op) => {
                json!({"op":op.name(),"pending":true,"accepted":null,"durable":false,"code":code})
            }
        }
    }
}
impl Service {
    pub(crate) async fn cache_intent(
        self: &Arc<Self>,
        id: String,
        command: zerus_client_core::desktop::cache_maintenance::Command,
    ) -> Result<Value, String> {
        if !command.valid(&id) {
            return Err("Invalid cache request identity.".into());
        }
        Ok(submit(
            self.shared.clone(),
            Input::Cache {
                id,
                command: Box::new(command),
            },
        )
        .await)
    }
    pub async fn automatic_approvals(
        self: &Arc<Self>,
        id: String,
        enabled: bool,
        categories: Option<Vec<String>>,
    ) -> Result<Value, String> {
        if !canonical(&id) {
            return Err("Invalid approval request identity.".into());
        }
        Ok(submit(
            self.shared.clone(),
            Input::Approvals {
                id,
                enabled,
                categories,
            },
        )
        .await)
    }
    /// The person's global question route switch (zerus-sa9qn.32).
    pub async fn question_routes(self: &Arc<Self>, id: String, enabled: bool) -> Value {
        submit(self.shared.clone(), Input::QuestionRoutes { id, enabled }).await
    }
    /// The person's latch on one child's question route (zerus-sa9qn.50).
    pub async fn question_route_child(self: &Arc<Self>, request: Request) -> Value {
        submit(self.shared.clone(), Input::QuestionRouteChild(Box::new(request))).await
    }
    pub async fn maintenance_configure(
        self: &Arc<Self>,
        id: String,
        configuration: maintenance::Configuration,
        bundle_id: String,
    ) -> Result<Value, String> {
        if !canonical(&id) {
            return Err("Invalid maintenance request identity.".into());
        }
        Ok(submit(
            self.shared.clone(),
            Input::Maintenance {
                id,
                configuration: Box::new(configuration),
                bundle_id,
            },
        )
        .await)
    }
    pub async fn frontend_result(self: &Arc<Self>, operation: Operation) -> Value {
        submit(self.shared.clone(), Input::Result(operation)).await
    }
}
pub(super) async fn approval_command(
    service: Option<Arc<Service>>,
    request_id: String,
    enabled: bool,
    categories: Option<Vec<String>>,
) -> Result<Value, String> {
    if !canonical(&request_id) {
        return Err("Invalid approval request identity.".into());
    }
    if categories
        .as_deref()
        .is_some_and(|ids| zerus_client_core::approvals::automatic::Set::from_ids(ids).is_none())
    {
        return Err("Invalid approval categories.".into());
    }
    match service {
        Some(service) => {
            service
                .automatic_approvals(request_id, enabled, categories)
                .await
        }
        None => Ok(refused(
            &Request::AutomaticApprovals {
                request_id,
                enabled,
                categories,
            },
            "frontend_route_unavailable",
        )),
    }
}
/// The person's global question route switch (zerus-sa9qn.32): only through
/// this native frontend lane.
pub(super) async fn question_routes_command(
    service: Option<Arc<Service>>,
    request_id: String,
    enabled: bool,
) -> Result<Value, String> {
    if !canonical(&request_id) {
        return Err("Invalid question route request identity.".into());
    }
    match service {
        Some(service) => Ok(service.question_routes(request_id, enabled).await),
        None => Ok(refused(
            &Request::QuestionRoutes {
                request_id,
                enabled,
            },
            "frontend_route_unavailable",
        )),
    }
}
/// The person's latch on one child's question route (zerus-sa9qn.50): only
/// through this native frontend lane.
pub(super) async fn question_route_child_command(
    service: Option<Arc<Service>>,
    request: Request,
) -> Result<Value, String> {
    if !matches!(request, Request::QuestionRouteChild { .. }) || !canonical(request.id()) {
        return Err("Invalid question route request identity.".into());
    }
    match service {
        Some(service) => Ok(service.question_route_child(request).await),
        None => Ok(refused(&request, "frontend_route_unavailable")),
    }
}
async fn submit(shared: Arc<Shared>, input: Input) -> Value {
    submit_held(shared, input, Arc::new(())).await
}
async fn submit_held(shared: Arc<Shared>, input: Input, held: Arc<dyn Send + Sync>) -> Value {
    let operation = input.operation();
    let fallback = input.refusal("frontend_route_unavailable");
    // Bound workers BEFORE spawn_blocking; webview reloads cannot accumulate
    // an unbounded queue of native policy jobs. One pending intent per operation.
    let charge = match shared.rpc.admit(Role::Fleet) {
        Ok(charge) => charge,
        Err(_) => {
            return match input {
                Input::Result(op) => shared.frontend.view(op),
                _ => fallback,
            }
        }
    };
    tauri::async_runtime::spawn_blocking(move || {
        let invalid = input.refusal("invalid_maintenance_configuration");
        let requested = match input.request() {
            Ok(value) => value,
            Err(_) => return invalid,
        };
        let reservation = match shared.frontend.begin(operation, requested) {
            Begin::Immediate(value) => return value,
            Begin::Work(work) => work,
        };
        let resources: Arc<dyn Send + Sync> = Arc::new((charge, held));
        execute(&shared, reservation, resources)
    })
    .await
    .unwrap_or_else(|_| shared_fallback(operation))
}

pub(super) async fn import_page(
    shared: Arc<Shared>,
    page: zerus_client_core::desktop::pending_reads::Page,
    held: Arc<dyn Send + Sync>,
) -> Value {
    let input = {
        let state = shared
            .frontend
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match state.operations.get(&Operation::ImportPendingReads) {
            Some(entry) if !entry.complete => match &entry.request {
                Request::ImportPendingReads { page: original, .. } if **original == page => {
                    Input::Result(Operation::ImportPendingReads)
                }
                _ => {
                    return json!({"op":"import_pending_reads","accepted":false,
                    "durable":false,"pending":true,"code":"previous_frontend_request_unconfirmed"})
                }
            },
            _ => Input::ImportReads {
                id: uuid::Uuid::new_v4().to_string(),
                page: Box::new(page),
            },
        }
    };
    submit_held(shared, input, held).await
}
fn shared_fallback(operation: Operation) -> Value {
    // A failed worker may already have delivered its original intent. No
    // fabricated refusal releases the UI's uncertain identity.
    json!({"op":operation.name(),"accepted":null,"durable":false,"pending":true,"code":"frontend_request_unconfirmed"})
}
fn invocation(args: &[&str], input: Vec<u8>) -> Invocation {
    Invocation {
        args: args.iter().map(|s| (*s).into()).collect(),
        stdin: Some(input),
        timeout: RPC_WAIT,
        request_id: None,
    }
}
fn owned(
    shared: &Shared,
    relay: &relay::Connection,
    call: &Invocation,
    resources: Arc<dyn Send + Sync>,
) -> Result<Vec<u8>, &'static str> {
    let hgs = relay.executable()?;
    run::run_owned(
        hgs,
        call,
        &[],
        &shared.stopped,
        |command| command.spawn(),
        resources,
    )
    .map_err(|_| "frontend_transport_unavailable")
}
fn help(
    shared: &Shared,
    relay: &relay::Connection,
    resources: Arc<dyn Send + Sync>,
    operation: Operation,
    lookup: bool,
) -> Result<(), &'static str> {
    let bytes = owned(shared, relay, &invocation(&["--help"], vec![]), resources)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "incompatible_frontend_cli")?;
    let marker = |wanted| text.lines().any(|line| line.trim() == wanted);
    if !marker(HELP)
        || (!lookup && !marker(ui_requests::HELP_MARKER))
        || (operation == Operation::MaintenanceConfigure && !marker(maintenance::HELP_MARKER))
        || (operation == Operation::CacheMaintenance
            && !marker(zerus_client_core::desktop::cache_maintenance::HELP_MARKER))
        || (operation == Operation::ImportPendingReads
            && !marker(zerus_client_core::desktop::pending_reads::HELP_MARKER))
    {
        return Err("incompatible_frontend_cli");
    }
    Ok(())
}
/// Presentation gates only; never a native permission or a writer grant.
fn submission_ready(snapshot: &Snapshot, request: &Request) -> bool {
    if matches!(
        request,
        Request::NotificationControl { .. }
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
            | Request::WorktreeProgramStop { .. }
            | Request::WorktreeProgramCleanup { .. }
            | Request::TerminationOutcome { .. }
    ) {
        return false;
    }
    if !snapshot.connected || snapshot.epoch.is_none() {
        return false;
    }
    // Reducing consent must still reach the authenticated route while its
    // background component is degraded or absent. The store commits it there.
    // A category change also goes there: the service accepts one that only
    // narrows (or one made while approvals are off) without full readiness
    // and refuses one that widens what is approved.
    if matches!(
        request,
        Request::AutomaticApprovals { enabled: false, .. }
            | Request::AutomaticApprovals {
                categories: Some(_),
                ..
            }
    ) {
        return true;
    }
    if matches!(request, Request::CacheMaintenance { .. }) {
        return snapshot.component_ready("cache_maintenance");
    }
    // The person's question route switches are Store rows; they need no
    // background worker, so turning routing off always reaches the service.
    if matches!(
        request,
        Request::QuestionRoutes { .. } | Request::QuestionRouteChild { .. }
    ) {
        return true;
    }
    snapshot.component_ready(if Operation::of(request) == Operation::AutomaticApprovals {
        "approvals"
    } else {
        "fleet"
    })
}
fn execute(
    shared: &Shared,
    mut reservation: Reservation,
    resources: Arc<dyn Send + Sync>,
) -> Value {
    let operation = Operation::of(&reservation.request);
    let mut dispatched = false;
    let result = (|| -> Result<Receipt, &'static str> {
        if !shared.allowed || shared.stopped.load(Ordering::Acquire) {
            return Err("frontend_route_unavailable");
        }
        if !reservation.lookup && operation != Operation::ImportPendingReads {
            let snapshot = shared.snapshot.lock().clone();
            if !submission_ready(&snapshot, &reservation.request) {
                return Err("desktop_background_unavailable");
            }
            if shared.responder.is_none() {
                return Err("frontend_identity_unavailable");
            }
        }
        // Native pair verification runs here, off the UI actor, and is retained
        // through preflight and spawn. A purported pair has no ordinary fallback.
        let relay = relay::Connection::select(&shared.hgs)?;
        help(
            shared,
            &relay,
            resources.clone(),
            operation,
            reservation.lookup,
        )?;
        if reservation.lookup {
            return lookup(shared, &relay, &reservation.request, resources);
        }
        let registration = shared
            .responder
            .as_ref()
            .ok_or("frontend_identity_unavailable")?
            .prepare_ui_request(
                uuid::Uuid::new_v4().to_string(),
                reservation.request.clone(),
            )
            .map_err(|_| "frontend_identity_unavailable")?;
        let input =
            serde_json::to_vec(registration.envelope()).map_err(|_| "invalid_frontend_request")?;
        if input.len() > zerus_watch_protocol::CONTROL_LINE_BYTES {
            return Err("frontend_request_too_large");
        }
        let held: Arc<dyn Send + Sync> = Arc::new((resources, registration.resources()));
        let spawned = Arc::new(AtomicBool::new(false));
        let original = spawned.clone();
        let call = invocation(&["desktop", "frontend-request", "--json"], input);
        let bytes = run::run_owned(
            relay.executable()?,
            &call,
            &[],
            &shared.stopped,
            move |command| {
                let child = command.spawn()?;
                original.store(true, Ordering::Release);
                // ALWAYS return the original Child, including when binding
                // refuses. The maintained owner must still stop/wait/drain it.
                let _ = registration.bind_original(&child);
                Ok(child)
            },
            held,
        );
        dispatched = spawned.load(Ordering::Acquire);
        if dispatched {
            reservation.record_original_exit(&bytes);
        }
        let bytes = bytes.map_err(|error| match error {
            OwnedError::StartFailed => "frontend_start_failed",
            _ => "frontend_request_unconfirmed",
        })?;
        serde_json::from_slice(&bytes)
            .map(Receipt::Present)
            .map_err(|_| "invalid_frontend_response")
    })();
    match result {
        Ok(receipt) => finish_receipt(reservation, receipt),
        Err(code) => {
            let complete = !reservation.lookup && !dispatched;
            let value = if complete {
                refused(&reservation.request, code)
            } else {
                unknown(&reservation.request, code)
            };
            reservation.finish(value, complete)
        }
    }
}
fn finish_receipt(reservation: Reservation, receipt: Receipt) -> Value {
    let resolved = match receipt {
        Receipt::Present(value) => checked(&reservation.request, value, reservation.lookup),
        Receipt::Absent if reservation.lookup && reservation.original_exit_confirmed => {
            // The service rechecks the exact caller's birth/liveness at the
            // mutation boundary and commits its receipt in that transaction.
            // After original-child exit, a verified absent receipt cannot be
            // followed by a late application of this envelope. A new intent
            // may be submitted; the original logical ID is never resubmitted.
            Some((
                refused(&reservation.request, "frontend_request_not_applied"),
                true,
            ))
        }
        Receipt::Absent => None,
    };
    let (value, complete) = resolved.unwrap_or_else(|| {
        (
            unknown(&reservation.request, "frontend_receipt_unconfirmed"),
            false,
        )
    });
    reservation.finish(value, complete)
}
fn lookup(
    shared: &Shared,
    relay: &relay::Connection,
    request: &Request,
    resources: Arc<dyn Send + Sync>,
) -> Result<Receipt, &'static str> {
    if matches!(
        request,
        Request::NotificationControl { .. }
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
            | Request::WorktreeProgramStop { .. }
            | Request::WorktreeProgramCleanup { .. }
            | Request::TerminationOutcome { .. }
    ) {
        return Err("frontend_receipt_unconfirmed");
    }
    let id = uuid::Uuid::new_v4().to_string();
    let operation = if Operation::of(request) == Operation::AutomaticApprovals {
        "delivery_result"
    } else if Operation::of(request) == Operation::QuestionRoutes {
        "question_routes_result"
    } else if Operation::of(request) == Operation::QuestionRouteChild {
        "question_route_child_result"
    } else if Operation::of(request) == Operation::ImportPendingReads {
        "pending_reads_import_result"
    } else if Operation::of(request) == Operation::CacheMaintenance {
        "cache_maintenance_result"
    } else {
        "maintenance_result"
    };
    let mut input = json!({"op":operation,"request_id":id,"original_request_id":request.id()});
    if let Request::ImportPendingReads { page, .. } = request {
        input["page"] = json!(page);
    }
    let bytes = owned(
        shared,
        relay,
        &invocation(
            &["desktop", "request", "--json"],
            serde_json::to_vec(&input).map_err(|_| "invalid_frontend_request")?,
        ),
        resources,
    )?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid_frontend_response")?;
    lookup_receipt(request, &id, value)
}
fn lookup_receipt(request: &Request, id: &str, value: Value) -> Result<Receipt, &'static str> {
    if matches!(
        request,
        Request::NotificationControl { .. }
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
            | Request::WorktreeProgramStop { .. }
            | Request::WorktreeProgramCleanup { .. }
            | Request::TerminationOutcome { .. }
    ) {
        return Err("frontend_receipt_unconfirmed");
    }
    if value["request_id"] != id || value["original_request_id"] != request.id() {
        return Err("invalid_frontend_response");
    }
    if matches!(
        Operation::of(request),
        Operation::AutomaticApprovals
            | Operation::QuestionRoutes
            | Operation::QuestionRouteChild
            | Operation::ImportPendingReads
            | Operation::CacheMaintenance
    ) {
        let expected = if Operation::of(request) == Operation::CacheMaintenance {
            "cache_maintenance_result"
        } else if Operation::of(request) == Operation::ImportPendingReads {
            "pending_reads_import_result"
        } else if Operation::of(request) == Operation::QuestionRoutes {
            "question_routes_result"
        } else if Operation::of(request) == Operation::QuestionRouteChild {
            "question_route_child_result"
        } else {
            "delivery_result"
        };
        if value["op"] != expected || value["accepted"] != true || value["durable"] != false {
            return Err("frontend_receipt_unconfirmed");
        }
        match value["found"].as_bool() {
            Some(true) => {
                let mut result = value["result"].clone();
                if matches!(request, Request::CacheMaintenance { .. }) {
                    result["request_id"] = json!(request.id());
                }
                Ok(Receipt::Present(result))
            }
            Some(false) if value["result"].is_null() => Ok(Receipt::Absent),
            _ => Err("frontend_receipt_unconfirmed"),
        }
    } else {
        // Normal maintenance receipts retain the latest 4096 IDs. This
        // canonical frontend admits no further Configure while its original
        // is unconfirmed, so its own traffic cannot evict that receipt before
        // lookup. Unknown recovery edits have a separate never-evicted bucket.
        if value["accepted"] == false
            && value["code"] == "unknown_request"
            && value.get("durable").is_none_or(|durable| *durable == false)
            && value["configured"] != true
        {
            return Ok(Receipt::Absent);
        }
        if value["accepted"] != true || value["durable"] != true {
            return Err("frontend_receipt_unconfirmed");
        }
        let mut value = value;
        value["request_id"] = json!(request.id());
        Ok(Receipt::Present(value))
    }
}
fn checked(request: &Request, mut value: Value, lookup: bool) -> Option<(Value, bool)> {
    if matches!(
        request,
        Request::DesktopOwnership { .. }
            | Request::ActivationWait { .. }
            | Request::NotificationControl { .. }
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
            | Request::WorktreeProgramStop { .. }
            | Request::WorktreeProgramCleanup { .. }
            | Request::TerminationOutcome { .. }
    ) {
        // Their separate consumers validate different receipts and completion rules.
        return None;
    }
    if value["request_id"] != request.id() {
        return None;
    }
    // A category change carrying a switch value the service no longer has
    // is refused before its policy write (zerus-sz42x.2).
    let approval_refusal = matches!(request, Request::AutomaticApprovals { .. })
        && value["code"] == "approval_policy_changed";
    // The person's latch refuses these before any write (zerus-sa9qn.50).
    let latch_refusal = matches!(request, Request::QuestionRouteChild { .. })
        && matches!(
            value["code"].as_str(),
            Some("conflict" | "stale_identity" | "not_found" | "capacity" | "invalid_request")
        );
    if !lookup
        && value["accepted"] == false
        && value["durable"] == false
        && (approval_refusal
            || latch_refusal
            || matches!(
                value["code"].as_str(),
                Some(
                    "frontend_authentication_refused"
                        | "frontend_identity_expired"
                        | "desktop_background_unavailable"
                        | "invalid_request_id"
                        | "delivery_stopping"
                )
            ))
    {
        // These refusals precede the mutation boundary. Store/configuration
        // failures may straddle a commit and remain unknown until lookup.
        value["op"] = json!(request.operation());
        value["pending"] = json!(false);
        return Some((value, true));
    }
    if let Request::CacheMaintenance { command, .. } = request {
        zerus_client_core::desktop::cache_maintenance::Status::response(
            &value,
            request.id(),
            command,
        )?;
        // This native packet is durable, while the fixed service work may
        // remain pending. Its consumer queries only the ORIGINAL operation ID.
        return Some((value, true));
    }
    if value["accepted"] != true || value["durable"] != true {
        return None;
    }
    let complete = match request {
        Request::ImportPendingReads { page, .. } => {
            if value["op"] != "import_pending_reads"
                || value["source"] != json!(page.source)
                || value["after"] != page.after
                || value["through"] != page.through()
                || !value["superseded"]
                    .as_u64()
                    .is_some_and(|count| count <= page.marks.len() as u64)
                || value["delivered"] != false
            {
                return None;
            }
            true
        }
        Request::DesktopOwnership { .. }
        | Request::DesktopOwnershipObserve { .. }
        | Request::ActivationWait { .. }
        | Request::CacheMaintenance { .. }
        | Request::NotificationControl { .. }
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
        | Request::WorktreeProgramStop { .. }
        | Request::WorktreeProgramCleanup { .. }
        | Request::TerminationOutcome { .. } => return None,
        // The service's own receipt of exactly this child, value and the
        // revision after the expected one (zerus-sa9qn.50).
        Request::QuestionRouteChild {
            session_id,
            person_off,
            expected_revision,
            ..
        } => {
            let applied = value["applied"].as_bool()?;
            if value["op"] != "question_route_child"
                || value["session_id"] != *session_id
                || value["person_off"] != *person_off
                || value["revision"].as_u64() != expected_revision.checked_add(u64::from(applied))
            {
                return None;
            }
            true
        }
        // The service's own receipt of exactly this switch value.
        Request::QuestionRoutes { enabled, .. } => {
            if value["op"] != "question_routes"
                || value["enabled"] != *enabled
                || !value["revision"].as_u64().is_some()
            {
                return None;
            }
            true
        }
        Request::AutomaticApprovals {
            enabled,
            categories,
            ..
        } => {
            if value["op"] != "automatic_approvals"
                || value["enabled"] != *enabled
                || categories
                    .as_ref()
                    .is_some_and(|ids| value["categories"] != json!(ids))
            {
                return None;
            }
            true
        }
        Request::MaintenanceConfigure { recovery, .. } => {
            if value["original_request_id"] != request.id()
                || value["configured"] != true
                || !value["revision"].as_u64().is_some_and(|n| n > 0)
                || !value["recovery_save"].is_string()
            {
                return None;
            }
            let edit = recovery
                .as_ref()
                .is_some_and(|recovery| recovery.policy.is_some());
            match value["recovery_save"].as_str() {
                Some("not_requested") if !edit => true,
                Some("saved") if edit => true,
                Some("unknown") if edit => false,
                _ => return None,
            }
        }
    };
    value["op"] = json!(request.operation());
    value["pending"] = json!(!complete);
    recovery_edit(request, &mut value);
    Some((value, complete))
}

/// Both native subsystems, with no one-shot edit. Opening a page reads this
/// value; only an explicit Configure submits it to the authenticated sender.
#[tauri::command]
pub async fn desktop_service_maintenance_configuration(
    source: tauri::State<'_, crate::commands::AppState>,
    updates: tauri::State<'_, crate::updates::Updates>,
    recovery: tauri::State<'_, crate::recovery::Recovery>,
) -> Result<maintenance::Configuration, String> {
    if !mode() || source.desktop_service().is_none() {
        return Err(MAINTENANCE_UNAVAILABLE.into());
    }
    let configuration = maintenance::Configuration {
        update: Some(updates.configuration_for_intent().await?),
        recovery: Some(recovery.native_configuration()),
    };
    configuration
        .valid()
        .then_some(configuration)
        .ok_or_else(|| MAINTENANCE_UNAVAILABLE.into())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn desktop_service_maintenance_configure(
    source: tauri::State<'_, crate::commands::AppState>,
    identity: tauri::State<'_, crate::identity::Identity>,
    request_id: String,
    configuration: maintenance::Configuration,
) -> Result<Value, String> {
    if !canonical(&request_id) {
        return Err("Invalid maintenance request identity.".into());
    }
    let Some(service) = source.desktop_service().filter(|_| mode()) else {
        return Ok(
            json!({"request_id":request_id,"op":"maintenance_configure","accepted":false,"durable":false,"pending":false,"code":"frontend_route_unavailable"}),
        );
    };
    service
        .maintenance_configure(request_id, configuration, identity.bundle_id.clone())
        .await
}
#[tauri::command]
pub async fn desktop_service_frontend_result(
    source: tauri::State<'_, crate::commands::AppState>,
    operation: Operation,
) -> Result<Value, String> {
    Ok(match source.desktop_service().filter(|_| mode()) {
        Some(service) => service.frontend_result(operation).await,
        None => json!({"op":operation.name(),"pending":false}),
    })
}

#[cfg(all(test, unix))]
pub(crate) mod tests;
