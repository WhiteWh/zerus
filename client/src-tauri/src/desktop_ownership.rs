//! Original-child ownership intents. Display state cannot release native writers.
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, PoisonError,
    },
    time::{Duration, Instant},
};
use zerus_client_core::{
    desktop::{
        drain::{Closed, Gate, Observation, Role},
        ownership::{self as wire, Action, Mode, Outcome, Query, Receipt, Status},
        relay,
        ui_requests::{self, Request},
        Responder,
    },
    run::{run_owned_observed, OwnedError},
    Invocation,
};

const WAIT: Duration = Duration::from_secs(5);
const GRANT_RETRIES: usize = 2;
const GRANT_BACKOFF: Duration = Duration::from_millis(250);

mod diagnostic;
pub(crate) use diagnostic::Phase;

#[derive(Clone, Default, Debug, Serialize)]
pub(crate) struct View {
    pub status: Option<Status>,
    pub request_id: Option<String>,
    pub pending: bool,
    pub running: bool,
    pub code: String,
}
struct Intent {
    request: Request,
    configuration: String,
    route: Arc<relay::Connection>,
    exit_confirmed: bool,
    complete: bool,
    receipt: Option<Receipt>,
}
#[derive(Default)]
struct State {
    status: Option<Status>,
    intent: Option<Intent>,
    // One uncertain initial native intent may be superseded by one fresh,
    // authenticated authority reduction; it is never called refused/applied.
    retired_native: Option<Intent>,
    legacy: Option<Arc<relay::Connection>>,
    running: bool,
    code: String,
    // First failing step wins over later read-only lookup failures. Only closed
    // local codes are kept, never child output, IDs, paths or configuration.
    diagnostic: Option<(&'static str, &'static str)>,
}
pub(crate) struct Controller {
    hgs: PathBuf,
    responder: Option<Arc<Responder>>,
    mode: Mode,
    gate: Gate,
    stop: Arc<AtomicBool>,
    state: Mutex<State>,
    diagnostic_publication: Mutex<()>,
}
impl Controller {
    pub(crate) fn new(hgs: PathBuf, responder: Option<Arc<Responder>>, mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            hgs,
            responder,
            mode,
            gate: Gate::new(2).unwrap(),
            stop: Arc::new(AtomicBool::new(false)),
            state: Mutex::new(State::default()),
            diagnostic_publication: Mutex::new(()),
        })
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
    pub(crate) fn view(&self) -> View {
        let state = self.state();
        Self::state_view(&state)
    }
    fn state_view(state: &State) -> View {
        View {
            status: state.status.clone(),
            request_id: state.intent.as_ref().map(|i| i.request.id().into()),
            pending: state.intent.as_ref().is_some_and(|i| !i.complete),
            running: state.running,
            code: state.code.clone(),
        }
    }
    /// Called only by the existing blocking ownership worker/publication task.
    /// This private record is an observation and cannot admit any operation.
    pub(crate) fn record_view(&self, phase: Phase) -> View {
        // Serialize capture plus replacement so a delayed older publication
        // cannot overwrite a newer observation from this controller.
        let _publication = self
            .diagnostic_publication
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let state = self.state();
        let view = Self::state_view(&state);
        let first = state.diagnostic;
        drop(state);
        diagnostic::record(phase, &view, first);
        view
    }
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.gate.close();
    }
    pub(crate) fn close(&self) -> Closed {
        self.gate.close()
    }
    pub(crate) fn unsupported(&self) {
        self.state().code = "incompatible_ownership_cli".into();
    }
    /// Admission precedes spawn_blocking; unknown original pipes keep their charge.
    pub(crate) async fn request(
        self: &Arc<Self>,
        action: Option<Action>,
        configuration: Option<String>,
        cancel: Option<Arc<AtomicBool>>,
    ) -> View {
        let Some(job) = self.admit(action, configuration, cancel) else {
            return self.view();
        };
        let owner = self.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || job.run()).await;
        owner.view()
    }
    /// Image replacement can need another bounded drain after the actor's
    /// 750 ms attempt. Only a matched, definitive refusal admits a NEW Grant;
    /// uncertain originals remain on the read-only result path.
    pub(crate) async fn startup_grant(
        self: &Arc<Self>,
        configuration: String,
        cancel: Arc<AtomicBool>,
    ) -> View {
        for attempt in 0..=GRANT_RETRIES {
            let view = self
                .request(
                    Some(Action::Grant),
                    Some(configuration.clone()),
                    Some(cancel.clone()),
                )
                .await;
            let retryable = self.retryable_grant();
            if !retryable || attempt == GRANT_RETRIES {
                return view;
            }
            let until = tokio::time::Instant::now() + GRANT_BACKOFF;
            while tokio::time::Instant::now() < until {
                if self.stop.load(Ordering::Acquire) || cancel.load(Ordering::Acquire) {
                    return self.view();
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
        unreachable!("the final bounded attempt returns its view")
    }
    /// Only a complete original refusal, or capacity before an uncertain
    /// original exists, can admit another fresh startup Grant.
    pub(crate) fn retryable_grant(&self) -> bool {
        let state = self.state();
        (!state.running && state.code == "ownership_transport_busy"
                    && state.intent.as_ref().is_none_or(|intent| intent.complete))
                    || state.intent.as_ref().is_some_and(|intent| {
                    intent.complete && intent.exit_confirmed
                        && matches!(intent.request, Request::DesktopOwnership { action: Action::Grant, .. })
                        && intent.receipt.as_ref().is_some_and(|receipt| {
                            !receipt.accepted && !receipt.durable
                                && state.status.as_ref() == Some(&receipt.current)
                                && matches!(&receipt.outcome, Outcome::Refused { code }
                                    if (state.code.is_empty() || state.code == *code)
                                        && matches!(code.as_str(), "ownership_drain_pending" | "ownership_resources_pending" | "ownership_transport_busy"))
                        })
                })
    }
    fn admit(
        self: &Arc<Self>,
        action: Option<Action>,
        configuration: Option<String>,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Option<Job> {
        if self.stop.load(Ordering::Acquire) {
            return None;
        }
        if self.responder.is_none() {
            self.state().code = "ownership_identity_unavailable".into();
            return None;
        }
        let charge = match self.gate.admit(Role::Fleet) {
            Ok(charge) => charge,
            Err(_) => {
                self.state().code = "ownership_transport_busy".into();
                return None;
            }
        };
        let mut state = self.state();
        if state.running {
            return None;
        }
        state.running = true;
        drop(state);
        Some(Job {
            owner: self.clone(),
            action,
            configuration,
            cancel: cancel.unwrap_or_else(|| self.stop.clone()),
            resources: Arc::new(charge),
            deadline: None,
            refresh_absent_native: false,
        })
    }
    pub(crate) fn granted(&self) -> bool {
        let state = self.state();
        state
            .intent
            .as_ref()
            .and_then(|i| i.receipt.as_ref())
            .is_some_and(|r| r.service_granted() && state.status.as_ref() == Some(&r.current))
    }
}

struct Job {
    owner: Arc<Controller>,
    action: Option<Action>,
    configuration: Option<String>,
    cancel: Arc<AtomicBool>,
    resources: Arc<dyn Send + Sync>,
    deadline: Option<Instant>,
    refresh_absent_native: bool,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.owner.state().running = false;
    }
}
fn invocation(args: &[&str], input: Vec<u8>) -> Invocation {
    Invocation {
        args: args.iter().map(|s| (*s).into()).collect(),
        stdin: Some(input),
        timeout: WAIT,
        request_id: None,
    }
}
impl Job {
    fn failure(&self, step: &'static str, error: &OwnedError, stderr: Option<&str>) {
        self.owner
            .state()
            .diagnostic
            .get_or_insert((step, failure_code(error, stderr)));
    }
    fn stopped(&self) -> bool {
        self.owner.stop.load(Ordering::Acquire) || self.cancel.load(Ordering::Acquire)
    }
    fn bound(&self, call: &mut Invocation) -> Result<(), &'static str> {
        if let Some(until) = self.deadline {
            let remaining = until
                .checked_duration_since(Instant::now())
                .filter(|time| !time.is_zero())
                .ok_or("ownership_takeover_unconfirmed")?;
            call.timeout = call.timeout.min(remaining);
        }
        Ok(())
    }
    fn call(
        &self,
        route: &Arc<relay::Connection>,
        args: &[&str],
        input: Vec<u8>,
    ) -> Result<Vec<u8>, &'static str> {
        if self.stopped() {
            return Err("ownership_stopped");
        }
        let held: Arc<dyn Send + Sync> =
            Arc::new((self.resources.clone(), self.owner.clone(), route.clone()));
        let mut call = invocation(args, input);
        self.bound(&mut call)?;
        let step = match args {
            ["--help"] => "native_takeover.help",
            ["desktop", "ownership-status", "--json"] => "native_takeover.status",
            _ => "native_takeover.lookup",
        };
        let result = run_owned_observed(
            route.executable()?,
            &call,
            &[],
            &self.cancel,
            |c| c.spawn(),
            held,
            |error, stderr| self.failure(step, error, stderr),
        )
        .map_err(|_| "ownership_transport_unavailable")?;
        route.executable()?;
        if self.stopped() || (args != ["--help"] && result.len() > wire::BYTES) {
            return Err("invalid_ownership_response");
        }
        Ok(result)
    }
    fn run(&self) {
        if self.owner.mode == Mode::Service {
            self.owner.record_view(Phase::WorkerStarted);
        }
        if let Err(code) = self.execute() {
            self.owner.state().code = code.into();
        }
    }
    fn execute(&self) -> Result<(), &'static str> {
        if self.stopped() {
            return Err("ownership_stopped");
        }
        if self.action.is_some_and(|a| a.mode() != self.owner.mode) {
            return Err("ownership_mode_mismatch");
        }
        let pending = {
            let state = self.owner.state();
            state.intent.as_ref().filter(|i| !i.complete).map(|i| {
                (
                    i.request.clone(),
                    i.configuration.clone(),
                    i.exit_confirmed,
                    i.route.clone(),
                )
            })
        };
        let route = match &pending {
            Some((_, _, _, route)) => route.clone(),
            None => Arc::new(relay::Connection::select(&self.owner.hgs)?),
        };
        let help = self.call(&route, &["--help"], vec![])?;
        let text = std::str::from_utf8(&help).map_err(|_| "incompatible_ownership_cli")?;
        let has = |marker: &str| text.lines().any(|line| line.trim() == marker);
        if !has(wire::HELP_MARKER)
            && pending.is_none()
            && self.owner.mode == Mode::Native
            && self.action == Some(Action::NativeTakeover)
            && self.owner.state().retired_native.is_none()
        {
            // Existing Unsupported compatibility, only before any ownership
            // intent. A disappeared marker cannot erase an uncertain original.
            self.owner.state().legacy = Some(route);
            return Err("ownership_unsupported");
        }
        if ![wire::HELP_MARKER, ui_requests::HELP_MARKER]
            .iter()
            .all(|m| has(m))
        {
            return Err("incompatible_ownership_cli");
        }
        if let Some((request, configuration, exit_confirmed, _)) = pending {
            // A fresh query is made AFTER observing the original child's exit.
            // Never reuse a negative lookup received before that observation.
            let query = Query::for_request(
                uuid::Uuid::new_v4().to_string(),
                configuration.clone(),
                &request,
            )
            .ok_or("invalid_ownership_query")?;
            let bytes = self.call(
                &route,
                &["desktop", "ownership-result", "--json"],
                serde_json::to_vec(&query).map_err(|_| "invalid_ownership_query")?,
            )?;
            let lookup: wire::Lookup =
                serde_json::from_slice(&bytes).map_err(|_| "invalid_ownership_response")?;
            if !lookup.matches(&query) {
                return Err("invalid_ownership_response");
            }
            if let Some(receipt) = lookup.receipt {
                return self.settle(receipt, &request, &configuration);
            }
            let may_refresh = {
                let state = self.owner.state();
                state.retired_native.is_none()
                    && state
                        .intent
                        .as_ref()
                        .is_some_and(|intent| intent.receipt.is_none())
            };
            if self.refresh_absent_native
                && may_refresh
                && exit_confirmed
                && self.owner.mode == Mode::Native
                && matches!(
                    &request,
                    Request::DesktopOwnership {
                        action: Action::NativeTakeover,
                        journal: None,
                        revision: 0,
                        ..
                    }
                )
            {
                let bytes =
                    self.call(&route, &["desktop", "ownership-status", "--json"], vec![])?;
                let status: Status =
                    serde_json::from_slice(&bytes).map_err(|_| "invalid_ownership_status")?;
                if status.matches(&configuration)
                    && status.journal.is_none()
                    && status.revision == 0
                    && status.state == wire::State::Passive
                    && !status.rearm_required
                    && status.request_id.is_none()
                    && status.owner_epoch.is_none()
                    && !status.identity_current
                {
                    // This observation releases NO writer. It admits at most
                    // one new Root-authenticated NativeTakeover, whose exact
                    // original result and actual resource drain must settle.
                    let mut state = self.owner.state();
                    state.retired_native = state.intent.take();
                    state.status = Some(status);
                    state.code = "ownership_fresh_native_takeover".into();
                    return Ok(());
                }
            }
            let mut state = self.owner.state();
            if lookup.definitive
                && exit_confirmed
                && matches!(
                    &request,
                    Request::DesktopOwnership {
                        journal: Some(_),
                        ..
                    }
                )
            {
                if let Some(intent) = &mut state.intent {
                    intent.complete = true;
                }
                state.code = "ownership_not_applied".into();
            } else {
                state.code = "ownership_unconfirmed".into();
            }
            return Ok(());
        }
        let bytes = self.call(&route, &["desktop", "ownership-status", "--json"], vec![])?;
        let status: Status =
            serde_json::from_slice(&bytes).map_err(|_| "invalid_ownership_status")?;
        let expected = self
            .configuration
            .as_deref()
            .unwrap_or(&status.configuration);
        if !status.matches(expected) {
            return Err("ownership_configuration_changed");
        }
        let responder = self
            .owner
            .responder
            .as_ref()
            .ok_or("ownership_identity_unavailable")?;
        match responder.cached_configuration() {
            Some(current) if current != status.configuration => {
                return Err("ownership_configuration_changed")
            }
            None if !responder.configure_once(status.configuration.clone()) => {
                return Err("ownership_configuration_changed");
            }
            _ => (),
        }
        self.owner.state().status = Some(status.clone());
        let Some(action) = self.action else {
            self.owner.state().code.clear();
            return Ok(());
        };
        if action == Action::Grant && status.rearm_required {
            self.owner.state().code = "ownership_rearm_required".into();
            return Ok(());
        }
        let request = Request::DesktopOwnership {
            request_id: uuid::Uuid::new_v4().to_string(),
            action,
            journal: status.journal.clone(),
            revision: status.revision,
        };
        // Reserve the exact logical intent before any original mutating child.
        let mut state = self.owner.state();
        // A fresh logical request gets its own first diagnostic. Read-only
        // lookups of an uncertain original keep that original's first failure.
        if self.owner.mode == Mode::Service {
            state.diagnostic = None;
        }
        state.intent = Some(Intent {
            request: request.clone(),
            configuration: status.configuration.clone(),
            route: route.clone(),
            exit_confirmed: false,
            complete: false,
            receipt: None,
        });
        drop(state);
        if self.owner.mode == Mode::Service {
            self.owner.record_view(Phase::FrontendRequestPrepared);
        }
        let registration =
            match responder.prepare_ui_request(uuid::Uuid::new_v4().to_string(), request.clone()) {
                Ok(registration) => registration,
                Err(_) => {
                    self.owner.state().intent.as_mut().unwrap().complete = true;
                    return Err("ownership_identity_unavailable");
                }
            };
        let input = match serde_json::to_vec(registration.envelope()) {
            Ok(input) => input,
            Err(_) => {
                self.owner.state().intent.as_mut().unwrap().complete = true;
                return Err("invalid_ownership_request");
            }
        };
        let helper = match route.executable() {
            Ok(helper) => helper,
            Err(code) => {
                self.owner.state().intent.as_mut().unwrap().complete = true;
                return Err(code);
            }
        };
        let held: Arc<dyn Send + Sync> = Arc::new((
            self.resources.clone(),
            self.owner.clone(),
            route.clone(),
            registration.resources(),
        ));
        let spawned = Arc::new(AtomicBool::new(false));
        let original = spawned.clone();
        let mut call = invocation(&["desktop", "frontend-request", "--json"], input);
        if let Err(code) = self.bound(&mut call) {
            self.owner.state().intent.as_mut().unwrap().complete = true;
            return Err(code);
        }
        let result = run_owned_observed(
            helper,
            &call,
            &[],
            &self.cancel,
            move |command| {
                let child = command.spawn()?;
                original.store(true, Ordering::Release);
                let _ = registration.bind_original(&child);
                Ok(child) // Always return the original, including a refused binding.
            },
            held,
            |error, stderr| self.failure("native_takeover.frontend_request", error, stderr),
        );
        {
            let mut state = self.owner.state();
            let intent = state.intent.as_mut().unwrap();
            if spawned.load(Ordering::Acquire) {
                intent.exit_confirmed = !matches!(result, Err(OwnedError::ExitUnverified));
            } else {
                intent.complete = true;
            }
        }
        let bytes = result.map_err(|_| "ownership_unconfirmed")?;
        route.executable()?;
        if self.stopped() || bytes.len() > wire::BYTES {
            return Err("ownership_unconfirmed");
        }
        let receipt: Receipt =
            serde_json::from_slice(&bytes).map_err(|_| "ownership_unconfirmed")?;
        self.settle(receipt, &request, &status.configuration)
    }
    fn settle(
        &self,
        receipt: Receipt,
        request: &Request,
        configuration: &str,
    ) -> Result<(), &'static str> {
        if !receipt.matches(request, configuration) {
            return Err("ownership_receipt_mismatch");
        }
        let mut state = self.owner.state();
        state.code = match &receipt.outcome {
            Outcome::Pending => "ownership_unconfirmed".into(),
            Outcome::Refused { code } => code.clone(),
            Outcome::Granted { .. } if receipt.service_granted() => String::new(),
            _ if receipt.native_ready() => String::new(),
            _ => "ownership_changed".into(),
        };
        state.status = Some(receipt.current.clone());
        let intent = state.intent.as_mut().ok_or("ownership_intent_missing")?;
        intent.complete = !matches!(receipt.outcome, Outcome::Pending);
        intent.receipt = Some(receipt);
        Ok(())
    }
}

/// Not serializable or constructible from a webview/status snapshot.
pub(crate) struct NativeReady {
    helper: Arc<relay::Connection>,
    // None only for a selected CLI's successful marker-absent legacy probe.
    _receipt: Option<Receipt>,
}
impl NativeReady {
    pub(crate) fn helper(&self) -> Result<&Path, &'static str> {
        self.helper.executable()
    }
}
/// Called after canonical claim, before Bootstrap or notifier construction.
pub(crate) fn native_takeover(
    hgs: PathBuf,
    responder: Arc<Responder>,
) -> Result<NativeReady, &'static str> {
    let owner = Controller::new(hgs, Some(responder), Mode::Native);
    let result = native_startup(owner.clone(), Instant::now() + Duration::from_secs(15));
    if let Err(code) = result {
        let diagnostic = owner
            .state()
            .diagnostic
            .unwrap_or(("native_takeover", code));
        crate::startup_error::record(diagnostic.0, diagnostic.1);
    }
    result
}

fn failure_code(error: &OwnedError, stderr: Option<&str>) -> &'static str {
    // Child text has no authority and is never written to the diagnostic.
    // Exact known messages select codes; arbitrary/partial text stays generic.
    if *error == OwnedError::ChildFailed {
        match stderr.map(str::trim) {
            Some("hgs: frontend process image unavailable") => {
                return "frontend_process_image_unavailable"
            }
            Some("hgs: frontend process identity unavailable") => {
                return "frontend_process_identity_unavailable"
            }
            Some("hgs: agent-descended frontend request refused") => {
                return "agent_descended_frontend_refused"
            }
            Some("hgs: canonical frontend evidence unavailable") => {
                return "canonical_frontend_evidence_unavailable"
            }
            _ => (),
        }
    }
    match error {
        OwnedError::InvalidInput => "invalid_input",
        OwnedError::Cancelled => "cancelled",
        OwnedError::TimedOut => "timed_out",
        OwnedError::StartFailed => "start_failed",
        OwnedError::WriteFailed => "write_failed",
        OwnedError::ChildFailed => "child_failed",
        OwnedError::OutputTooLarge => "output_too_large",
        OwnedError::Incomplete => "incomplete",
        OwnedError::ExitUnverified => "exit_unverified",
    }
}
fn native_startup(owner: Arc<Controller>, deadline: Instant) -> Result<NativeReady, &'static str> {
    let mut action = Some(Action::NativeTakeover);
    loop {
        let mut job = owner
            .admit(action, None, None)
            .ok_or("ownership_transport_busy")?;
        job.deadline = Some(deadline);
        job.refresh_absent_native = true;
        job.run();
        drop(job);
        if Instant::now() >= deadline {
            return Err("ownership_takeover_unconfirmed");
        }
        let view = owner.view();
        let ready = {
            let state = owner.state();
            state
                .legacy
                .as_ref()
                .map(|route| (None, route.clone()))
                .or_else(|| {
                    state
                        .intent
                        .as_ref()
                        .and_then(|i| i.receipt.clone().map(|r| (r, i.route.clone())))
                        .filter(|(r, _)| {
                            r.native_ready() && state.status.as_ref() == Some(&r.current)
                        })
                        .map(|(receipt, route)| (Some(receipt), route))
                })
        };
        if let Some((receipt, helper)) = ready {
            if !matches!(owner.gate.close().wait_until(deadline), Observation::Idle) {
                return Err("ownership_resources_pending");
            }
            helper.executable()?;
            return Ok(NativeReady {
                helper,
                _receipt: receipt,
            });
        }
        let refresh = {
            let state = owner.state();
            state.intent.is_none() && state.retired_native.is_some()
        };
        if refresh {
            action = Some(Action::NativeTakeover);
            continue;
        }
        if !view.pending || Instant::now() >= deadline {
            return Err("ownership_takeover_unconfirmed");
        }
        action = None; // A pending original is queried, never resent.
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests;
