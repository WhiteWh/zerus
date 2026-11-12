//! `Connector.execute` and `_execute_claimed` (`connector.py:829-1235`) for
//! the operations this build implements: claim with the no-replay
//! fingerprint, route checks, validation, capability gates, the native calls
//! and their identity checks, then one journaled receipt. Operations of later
//! slices are refused as unsupported after the claim, so they never run.
//!
//! Before journaling, a new receipt is made acceptable to the relay
//! (divergence b): an error is cut to 4,096 code points, a non-string error
//! becomes a fixed text, and a receipt whose canonical form with its
//! request_id exceeds 1 MiB becomes a fixed failure without its result.
use crate::capabilities::Flags;
use crate::fleet::PeerNative;
use crate::inspection::bound_inspection;
use crate::journal::{is_mutation, JournalError, RESTART_ERROR};
use crate::native::{Call, Native, NativeError};
use crate::projects::{is_archived, launch_projects};
use crate::pyvalue::{bounded, int_literal, is_str, py_eq, py_len, PyError};
use crate::request::{self, receipt};
use crate::runtime::Connector;
use crate::validate::{self, Validated};
use serde_json::{json, Map, Value};
use std::cell::Cell;
use std::rc::Rc;
use zerus_relay::{json, protocol};

/// Python's catch-all receipt text for an unexpected exception.
pub const GENERIC: &str = "connector could not record native outcome";
pub const TOO_LARGE: &str = "native result exceeds relay size limit";
/// A native error that was not text, as the relay accepts only text.
pub const ERROR_NOT_TEXT: &str = "native command reported an error that was not text";
/// `hgs task exchange --json` imports and pages one project's tasks (seconds).
pub const TASK_EXCHANGE_TIMEOUT: f64 = 30.0;
/// `MAX_RESULT`: Python's plain-JSON bound of a receipt.
pub const MAX_RESULT: usize = 1024 * 1024;
/// The relay's bound of a canonical receipt with its request_id.
pub const MAX_RECEIPT: usize = 1024 * 1024;
pub const MAX_ERROR: usize = 4096;

/// Why an operation produced no result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Python's ConnectorError: its text reaches the receipt.
    Connector(String),
    /// Any other exception in Python: the generic text.
    Generic,
    /// The connector stopped before the call (divergence h).
    Stopped,
}

impl Failure {
    pub fn text(text: &str) -> Failure {
        Failure::Connector(text.into())
    }
    pub fn message(&self) -> &str {
        match self {
            Failure::Connector(text) => text,
            Failure::Generic => GENERIC,
            Failure::Stopped => RESTART_ERROR,
        }
    }
}

impl From<NativeError> for Failure {
    fn from(error: NativeError) -> Self {
        match error {
            NativeError::Failed(text) => Failure::text(text),
            NativeError::Unicode => Failure::Generic,
            NativeError::Stopping => Failure::Stopped,
            NativeError::SessionChanged => Failure::text(crate::session_selector::CHANGED),
        }
    }
}

/// `except ValueError as error: raise ConnectorError(str(error))`; any other
/// exception is the generic text.
pub(crate) fn py(error: PyError) -> Failure {
    match error {
        PyError::Value(text) => Failure::Connector(text),
        PyError::Type => Failure::Generic,
    }
}

/// A fatal journal failure stops the connector, as an uncaught sqlite3
/// error stops Python's.
#[derive(Debug)]
pub struct Fatal(pub JournalError);

/// The outcome of one delivered request.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Refused before any journal row ("Rejected relay request: ...").
    Rejected(String),
    /// A stored receipt queued again for upload, or a request in progress.
    Replayed,
    /// A new receipt was journaled.
    Finished(Value),
}

/// The receipt the journal stores for `response`, made acceptable to the
/// relay. `mutation` decides the state of a substitute.
pub fn sanitize(mut response: Value, request_id: &str, mutation: bool) -> Value {
    match response.get("error") {
        Some(Value::Null) | None => {}
        Some(Value::String(text)) if py_len(text) > MAX_ERROR => {
            let cut: String = text.chars().take(MAX_ERROR).collect();
            response["error"] = json!(cut);
        }
        Some(Value::String(_)) => {}
        Some(_) => response["error"] = json!(ERROR_NOT_TEXT),
    }
    if protocol::result(&response, request_id).is_err() {
        let state = if mutation { "uncertain" } else { "failed" };
        return receipt(state, Value::Null, Some(TOO_LARGE));
    }
    response
}

fn reject(text: &str) -> Outcome {
    Outcome::Rejected(text.into())
}

/// Where a routed request runs: on this computer, guarded by its machine
/// identity, or in a peer's context.
enum Target {
    Here(Option<String>),
    Peer(Rc<Connector<PeerNative>>),
}

impl<N: Native + 'static> Connector<N> {
    /// `Connector.execute(request)`.
    pub async fn execute(&self, request: Value) -> Result<Outcome, Fatal> {
        let Value::Object(request) = request else {
            return Ok(reject("invalid request"));
        };
        let request_id = match validate::checked_uuid(request.get("request_id")) {
            Ok(request_id) => request_id,
            Err(text) => return Ok(Outcome::Rejected(text)),
        };
        let journal_operation = request
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or("invalid")
            .to_owned();
        let request = Value::Object(request);
        let Ok(hash) = request::request_hash(&request) else {
            return Ok(reject("invalid native request JSON"));
        };
        let now = self.wall();
        match self
            .journal
            .claim(&request_id, &journal_operation, Some(&hash), now)
        {
            Ok(true) => {}
            Ok(false) => {
                self.journal.replay(&request_id).map_err(Fatal)?;
                return Ok(Outcome::Replayed);
            }
            Err(JournalError::Refused(error)) => return Ok(Outcome::Rejected(error.0)),
            Err(error) => return Err(Fatal(error)),
        }
        let request = match request {
            Value::Object(request) => request,
            _ => unreachable!(),
        };
        let routed = if (crate::session_projects::OPERATIONS.contains(&journal_operation.as_str())
            || zerus_relay::repositories::OPERATIONS.contains(&journal_operation.as_str()))
            && request.contains_key("gateway_route")
        {
            let route = &request["gateway_route"];
            let matches = route.as_object().is_some_and(|route| {
                request.get("computer_id").is_none_or(|computer| {
                    py_eq(route.get("computer_id").unwrap_or(&Value::Null), computer)
                }) && route.get("machine_id").is_some_and(|machine| {
                    request
                        .get("payload")
                        .is_some_and(|payload| payload.get("expected_machine_id") == Some(machine))
                })
            });
            if matches {
                self.resolve(route).await
            } else {
                Err(Failure::text(
                    if zerus_relay::repositories::OPERATIONS.contains(&journal_operation.as_str()) {
                        "The selected computer changed. Refresh computers before cloning."
                    } else {
                        "project assignment computer does not match its immutable route"
                    },
                ))
            }
        } else if journal_operation == "directory_create" && request.contains_key("gateway_route") {
            let route = &request["gateway_route"];
            let matches = route.as_object().is_some_and(|route| {
                request.get("computer_id").is_none_or(|computer| {
                    py_eq(route.get("computer_id").unwrap_or(&Value::Null), computer)
                })
            });
            if matches {
                self.resolve(route).await
            } else {
                Err(Failure::text(
                    "directory creation computer does not match its immutable route",
                ))
            }
        } else if let Some(route) = request.get("gateway_route") {
            self.resolve(route).await
        } else {
            if zerus_relay::repositories::OPERATIONS.contains(&journal_operation.as_str()) {
                match validate::checked_uuid(
                    request
                        .get("payload")
                        .and_then(|p| p.get("expected_machine_id")),
                ) {
                    Ok(machine) => Ok(Target::Here(Some(machine))),
                    Err(error) => Err(Failure::Connector(error)),
                }
            } else {
                Ok(Target::Here(None))
            }
        };
        let target = match routed {
            Ok(target) => target,
            Err(failure) => {
                let response = receipt("failed", Value::Null, Some(failure.message()));
                self.finish(&request_id, &response)?;
                return Ok(Outcome::Finished(response));
            }
        };
        let response = match target {
            Target::Here(None) => self.execute_claimed(&request, &request_id).await,
            Target::Here(Some(machine)) => {
                Connector::<N>::guarded(machine, self.execute_claimed(&request, &request_id)).await
            }
            Target::Peer(context) => context.execute_claimed(&request, &request_id).await,
        };
        let response = sanitize(response, &request_id, is_mutation(&journal_operation));
        self.finish(&request_id, &response)?;
        Ok(Outcome::Finished(response))
    }

    /// `Fleet.resolve`: an exact route to this computer or a direct peer,
    /// checked against the current inventory without waiting on any other
    /// peer.
    async fn resolve(&self, envelope: &Value) -> Result<Target, Failure> {
        let (route, machine) = crate::fleet::route_target(envelope).map_err(Failure::text)?;
        let fleet = match &self.fleet {
            Some(fleet) if self.gateway_available() => fleet,
            _ => return Err(Failure::text(crate::fleet::ROUTING_UNAVAILABLE)),
        };
        let Some(identity) = route else {
            if fleet.local_id.borrow().as_deref() != Some(machine.as_str()) {
                return Err(Failure::text(crate::fleet::LOCAL_CHANGED));
            }
            return Ok(Target::Here(Some(machine)));
        };
        let current = fleet.routes.borrow().get(&identity).cloned();
        if !current.is_some_and(|row| row.online && row.machine_id == machine) {
            return Err(Failure::text(crate::fleet::ROUTE_UNAVAILABLE));
        }
        let Some(context) = fleet.contexts.borrow().get(&identity).cloned() else {
            return Err(Failure::Generic);
        };
        if !context.snapshot_ready.is_set() {
            context.snapshot().await.map_err(Failure::Connector)?;
        }
        Ok(Target::Peer(context))
    }

    fn finish(&self, request_id: &str, response: &Value) -> Result<(), Fatal> {
        self.journal
            .finish(request_id, response, self.wall())
            .map_err(Fatal)
    }

    /// `_execute_claimed`: the receipt of a claimed request.
    async fn execute_claimed(&self, request: &Map<String, Value>, request_id: &str) -> Value {
        let selection = self.labels_selection(
            request
                .get("session")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            request.get("payload"),
        );
        crate::runtime::SESSION_LABEL_SELECTION
            .scope(selection, self.execute_claimed_inner(request, request_id))
            .await
    }
    async fn execute_claimed_inner(&self, request: &Map<String, Value>, request_id: &str) -> Value {
        if self.flags.get().windows_gateway
            && !request
                .get("operation")
                .and_then(Value::as_str)
                .is_some_and(|op| crate::capabilities::WINDOWS_OPERATIONS.contains(&op))
        {
            return receipt(
                "failed",
                Value::Null,
                Some("operation is unavailable on a Windows local gateway"),
            );
        }
        if crate::media::claims(request) {
            return self.media(request, request_id).await;
        }
        // A mutation that failed once its native write may have begun is
        // uncertain.
        let mutation_started = Cell::new(false);
        let outcome = async {
            let validated = self
                .validated(request, request_id)
                .await
                .map_err(Failure::Connector)?;
            // Refuse this new ABI before even an archive lookup on retained hgs.
            if validated.operation == "history_projection" && !self.flags.get().history_projection {
                return Err(Failure::text(
                    "native history projection ABI is unavailable",
                ));
            }
            if self.flags.get().windows_gateway
                && !validated.session.is_empty()
                && !validated
                    .payload
                    .as_ref()
                    .is_some_and(|p| p.get("archive_id").is_some())
            {
                self.windows_target(&validated).await?;
            }
            if self.flags.get().archive || self.flags.get().windows_gateway {
                if let Some(payload) = validated
                    .payload
                    .as_ref()
                    .filter(|payload| payload.get("archive_id").is_some())
                {
                    self.archive_preflight(&validated.session, payload).await?;
                }
            }
            self.gate(
                &validated.operation,
                validated.payload.as_ref(),
                self.flags.get(),
            )?;
            let result = if crate::mutations::handles(&validated.operation) {
                self.mutate(&validated, request_id, &mutation_started)
                    .await?
            } else if crate::launch::handles(&validated.operation) {
                self.create(&validated, request_id, &mutation_started)
                    .await?
            } else if ["task_send", "task_cancel"].contains(&validated.operation.as_str()) {
                self.task_action(&validated, &mutation_started).await?
            } else {
                self.run(&validated, request_id, &mutation_started).await?
            };
            if validated.operation == "analytics_read" {
                let local = self
                    .fleet
                    .as_ref()
                    .and_then(|fleet| fleet.local_id.borrow().clone());
                let machine = request
                    .get("gateway_route")
                    .and_then(|route| route["machine_id"].as_str())
                    .or(local.as_deref());
                protocol::analytics_summary(
                    &result,
                    validated.payload.as_ref().expect("validated"),
                    machine,
                )
                .map_err(|_| Failure::text("native analytics summary did not match"))?;
            }
            Ok((receipt_of(&validated.operation, result), validated))
        }
        .await;
        let failed = |failure: Failure| {
            let state = if mutation_started.get() {
                "uncertain"
            } else {
                "failed"
            };
            receipt(state, Value::Null, Some(failure.message()))
        };
        match outcome {
            Ok((response, validated)) => {
                match json::python_default_len(&response, true) {
                    Ok(size) if size <= MAX_RESULT => {
                        self.archived(&validated, &response);
                        response
                    }
                    Ok(_) => failed(Failure::text(TOO_LARGE)),
                    // A number beyond the double range: Python stores
                    // "Infinity", which no relay accepts.
                    Err(_) => failed(Failure::Generic),
                }
            }
            Err(failure) => failed(failure),
        }
    }

    /// `await asyncio.to_thread(self.validate, request)`: a send of
    /// megabytes is checked off the runtime thread.
    async fn validated(
        &self,
        request: &Map<String, Value>,
        request_id: &str,
    ) -> Result<Validated, String> {
        let (request, request_id, context) =
            (request.clone(), request_id.to_owned(), self.context());
        tokio::task::spawn_blocking(move || validate::validate(&request, &request_id, &context))
            .await
            .unwrap_or_else(|_| Err(GENERIC.into()))
    }

    /// `archive_preflight`: an archive no page, read or snapshot admits is
    /// checked natively before anything runs for it.
    async fn archive_preflight(&self, session: &str, payload: &Value) -> Result<(), Failure> {
        let archive = payload["archive_id"].as_str().unwrap_or_default();
        let windows = self.flags.get().windows_gateway;
        if !windows && self.context().admits(session, archive) {
            return Ok(());
        }
        let call = crate::archives::call(true, windows)
            .payload(json!({"name": session, "archive_id": archive}))
            .timeout(5.0);
        let detail = self.call(call).await?;
        if !crate::archives::lookup_matches(&detail, session, archive, payload) {
            return Err(Failure::text(crate::archives::LOOKUP_MISMATCH));
        }
        if windows && !crate::capabilities::windows_provider(&detail["cmd"]) {
            return Err(Failure::text(crate::capabilities::WINDOWS_PROVIDER_REFUSAL));
        }
        let now = self.clock();
        crate::archives::remember(
            &mut self.state.borrow_mut().paged_archives,
            [(session.to_owned(), archive.to_owned())],
            now,
        );
        Ok(())
    }

    /// Snapshot admission never substitutes for an authoritative provider and
    /// identity check just before dispatch. Duplicate receipts bypass this gate.
    async fn windows_target(&self, validated: &Validated) -> Result<(), Failure> {
        let detail = self
            .call(Call::new([
                "inspect",
                &validated.session,
                "--skip-processes",
            ]))
            .await?;
        if detail["name"].as_str() != Some(&validated.session)
            || !crate::capabilities::windows_provider(&detail["agent"])
            || detail.as_object().is_none_or(is_archived)
        {
            return Err(Failure::text(crate::capabilities::WINDOWS_PROVIDER_REFUSAL));
        }
        if let Some(payload) = &validated.payload {
            for (expected, field) in [
                ("expected_run_id", "run_id"),
                ("expected_conversation_id", "conversation_id"),
            ] {
                // An unconfirmed launch reports an explicit null conversation,
                // which clients address as "". A missing field stays unknown.
                if payload
                    .get(expected)
                    .is_some_and(|expected| match detail.get(field) {
                        Some(Value::Null) if field == "conversation_id" => {
                            expected.as_str() != Some("")
                        }
                        Some(actual) => expected != actual,
                        None => true,
                    })
                {
                    return Err(Failure::text("fresh native target identity did not match"));
                }
            }
        }
        Ok(())
    }

    /// After a completed request: a restore or forget revokes its archive, a
    /// read keeps an admitted one, and a page admits its archives.
    fn archived(&self, validated: &Validated, response: &Value) {
        if response["state"] != "completed" {
            return;
        }
        let now = self.clock();
        let mut state = self.state.borrow_mut();
        let operation = validated.operation.as_str();
        if let Some(archive) = validated
            .payload
            .as_ref()
            .and_then(|payload| payload.get("archive_id"))
            .and_then(Value::as_str)
        {
            let key = (validated.session.clone(), archive.to_owned());
            if matches!(operation, "restore" | "forget") {
                state.paged_archives.remove(&key);
            } else if protocol::READS.contains(&operation) {
                if let Some(at) = state.paged_archives.get_mut(&key) {
                    *at = now;
                }
            }
        }
        if operation == "archive_list" {
            let targets = crate::archives::targets(&response["result"]);
            crate::archives::remember(&mut state.paged_archives, targets, now);
        }
    }

    /// `hgs archives --json` for one query: a checked page with the project
    /// memberships of its rows and only the projects they use.
    async fn archive_page(&self, payload: &Value, request_id: &str) -> Result<Value, Failure> {
        let call = crate::archives::call(false, self.flags.get().windows_gateway)
            .payload(payload.clone())
            .timeout(30.0);
        let raw = self.call(call).await?;
        let mut page = crate::archives::page(&raw, payload).map_err(|error| match error {
            crate::archives::PageError::Value(text) => Failure::Connector(text),
            crate::archives::PageError::Other => Failure::Generic,
        })?;
        if self.flags.get().windows_gateway && !crate::archives::supported_page(&page) {
            return Err(Failure::text(crate::capabilities::WINDOWS_PROVIDER_REFUSAL));
        }
        let groups = self
            .state
            .borrow()
            .project_cache
            .clone()
            .filter(crate::pyvalue::truthy)
            .unwrap_or_else(|| crate::projects::unavailable(&self.settings.binding));
        if let Some(Value::Array(sessions)) = page.get_mut("sessions") {
            crate::projects::apply_memberships(sessions, &groups);
        }
        let used: Vec<Value> = page["sessions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|row| row.get("mobile_project_id").cloned().unwrap_or(Value::Null))
            .collect();
        let mut projects: Map<String, Value> = groups
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(key, _)| key.as_str() != "projects")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let listed = groups["projects"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .filter(|project| {
                let id = project.get("id").cloned().unwrap_or(Value::Null);
                used.iter().any(|value| crate::pyvalue::py_eq(value, &id))
            })
            .map(|project| {
                Value::Object(
                    project
                        .iter()
                        .filter(|(key, _)| matches!(key.as_str(), "id" | "name" | "color"))
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                )
            })
            .collect();
        projects.insert("projects".into(), Value::Array(listed));
        page["mobile_projects"] = Value::Object(projects);
        page["request_id"] = json!(request_id);
        Ok(page)
    }

    /// The capability checks of `_execute_claimed`, in Python's order.
    fn gate(&self, operation: &str, payload: Option<&Value>, flags: Flags) -> Result<(), Failure> {
        if flags.windows_gateway && !crate::capabilities::WINDOWS_OPERATIONS.contains(&operation) {
            return Err(Failure::text(
                "operation is unavailable on a Windows local gateway",
            ));
        }
        if operation == "clear_context"
            && payload.is_some_and(|payload| payload.get("dismiss_prompt").is_some())
            && !flags.clear_context_dismiss
        {
            return Err(Failure::text(
                "native clear-context dismiss ABI is unavailable",
            ));
        }
        if zerus_relay::repositories::OPERATIONS.contains(&operation) && !flags.repository {
            return Err(Failure::text(crate::repositories::UNAVAILABLE));
        }
        if operation == crate::git_status::OPERATION && !flags.git_status {
            return Err(Failure::text(crate::git_status::UNAVAILABLE));
        }
        if operation == "analytics_read" && !flags.analytics {
            return Err(Failure::text("native analytics read ABI is unavailable"));
        }
        if operation == "archive_list" && !flags.archive {
            return Err(Failure::text(crate::archives::UNAVAILABLE));
        }
        if crate::session_projects::OPERATIONS.contains(&operation) && !flags.session_projects {
            return Err(Failure::text(crate::session_projects::UNAVAILABLE));
        }
        if operation == "hooks_status" && !flags.hook_health {
            return Err(Failure::text(crate::hook_health::UNAVAILABLE));
        }
        if operation == "history" && !flags.history {
            return Err(Failure::text("native public history ABI is unavailable"));
        }
        let launch = crate::capabilities::LAUNCH.contains(&operation);
        if launch && operation != "directory_create" && !flags.launch {
            return Err(Failure::text("native exact launch-ID ABI is unavailable"));
        }
        if operation == "directory_create" && !flags.directory_create {
            return Err(Failure::text(
                "native explicit directory creation ABI is unavailable",
            ));
        }
        if crate::capabilities::WORKTREES.contains(&operation) && !flags.worktree {
            return Err(Failure::text("native mobile worktree ABI is unavailable"));
        }
        if operation == crate::session_notifications::OPERATION && !flags.session_notifications {
            return Err(Failure::text(
                "native shared notification ABI is unavailable",
            ));
        }
        if operation == crate::session_question_routing::OPERATION && !flags.question_routing {
            return Err(Failure::text(crate::session_question_routing::UNAVAILABLE));
        }
        if operation == crate::session_question_route_person::OPERATION
            && !flags.question_route_person
        {
            return Err(Failure::text(
                crate::session_question_route_person::UNAVAILABLE,
            ));
        }
        if operation == "mark_read" && !flags.read {
            return Err(Failure::text("native shared read ABI is unavailable"));
        }
        if operation == "task_exchange" && !flags.task_exchange {
            return Err(Failure::text("native task exchange ABI is unavailable"));
        }
        if ["task_send", "task_cancel", "task_done"].contains(&operation) && !flags.task_actions {
            return Err(Failure::text("native task actions ABI is unavailable"));
        }
        Ok(())
    }

    /// Send now or Cancel on the session's computer: a mutation, run once per
    /// identity by the journal. hgs's refusal before anything happened fails
    /// the receipt with its reason; any other outcome after the call began is
    /// uncertain.
    async fn task_action(
        &self,
        validated: &Validated,
        started: &Cell<bool>,
    ) -> Result<Value, Failure> {
        let action = if validated.operation == "task_send" {
            "mobile-send"
        } else {
            "mobile-cancel"
        };
        let call = Call::new(["task", action, "--json"])
            .payload(validated.payload.clone().expect("validated"))
            .failure_json();
        started.set(true);
        let answer = self.call(call).await?;
        task_answer(answer).map_err(|(failure, definite)| {
            if definite {
                started.set(false);
            }
            failure
        })
    }

    pub(crate) async fn call(&self, call: Call) -> Result<Value, Failure> {
        Ok(self.native(call).await?.json())
    }

    async fn run(
        &self,
        validated: &Validated,
        request_id: &str,
        mutation_started: &Cell<bool>,
    ) -> Result<Value, Failure> {
        let session = validated.session.as_str();
        let payload = validated.payload.as_ref();
        match validated.operation.as_str() {
            operation if zerus_relay::repositories::OPERATIONS.contains(&operation) => {
                let payload = payload.expect("validated");
                if operation == "repository_clone" {
                    mutation_started.set(true);
                }
                let result = self
                    .call(crate::repositories::call(operation, payload))
                    .await?;
                crate::repositories::project(operation, payload, &result).map_err(Failure::text)
            }
            crate::git_status::OPERATION => {
                let payload = payload.expect("validated");
                let result = self.call(crate::git_status::call(payload)).await?;
                crate::git_status::project(
                    &result,
                    request_id,
                    payload["path"].as_str().expect("validated"),
                )
                .map_err(Failure::text)
            }
            "analytics_read" => {
                let mut payload = payload.expect("validated").clone();
                payload["action"] = json!("summary");
                let mut call = Call::new(["analytics", "--json"])
                    .payload(payload)
                    .timeout(10.0);
                call.max_output_bytes = Some(MAX_RESULT);
                self.call(call).await
            }
            "archive_list" => {
                self.archive_page(payload.expect("validated"), request_id)
                    .await
            }
            "inspect" => self.inspect(session, payload).await,
            "process_output" => {
                self.processes("--output", session, payload.expect("validated"), request_id)
                    .await
            }
            "session_notifications" => {
                let payload = payload.expect("validated");
                let result = self
                    .call(
                        Call::new(["notifications", session, "--json"])
                            .payload(payload.clone())
                            .failure_json(),
                    )
                    .await?;
                if !crate::session_notifications::result_valid(&result, payload) {
                    return Err(Failure::text(
                        "native notification preference acknowledgement did not match",
                    ));
                }
                Ok(result)
            }
            "session_question_routing" => {
                let payload = payload.expect("validated");
                let result = self
                    .call(
                        Call::new(["question-routing", session, "--json"])
                            .payload(payload.clone())
                            .failure_json(),
                    )
                    .await?;
                if !crate::session_question_routing::result_valid(&result, payload) {
                    return Err(Failure::text(
                        "native question routing acknowledgement did not match",
                    ));
                }
                Ok(result)
            }
            crate::session_question_route_person::OPERATION => {
                let payload = payload.expect("validated");
                if self.peer {
                    return Err(Failure::text(crate::session_question_route_person::PEER));
                }
                let result = self
                    .call(
                        Call::new(["desktop", "request", "--json"])
                            .payload(crate::session_question_route_person::request(payload))
                            .failure_json(),
                    )
                    .await?;
                if !crate::session_question_route_person::result_valid(&result, payload) {
                    return Err(Failure::text(
                        "native person question route acknowledgement did not match",
                    ));
                }
                Ok(result)
            }
            "mark_read" => {
                let payload = payload.expect("validated");
                let result = self
                    .call(
                        Call::new(["read", session, "--json"])
                            .payload(payload.clone())
                            .failure_json(),
                    )
                    .await?;
                if !mark_read_result(&result, payload) {
                    return Err(Failure::text("native read acknowledgement did not match"));
                }
                Ok(result)
            }
            "project_assignment" | "project_assign" => {
                let payload = payload.expect("validated");
                if validated.operation == "project_assign" {
                    if self.stopping.is_set() {
                        return Err(Failure::Stopped);
                    }
                    // From the sole native write boundary, a lost/invalid acknowledgement
                    // is uncertain. The original claimed UUID is never executed again.
                    mutation_started.set(true);
                }
                let selected = crate::runtime::SESSION_LABEL_SELECTION
                    .try_with(Clone::clone)
                    .ok()
                    .flatten();
                let mut call =
                    crate::session_projects::call(&validated.operation, payload, session);
                if let Some(selected) = &selected {
                    call.payload.as_mut().unwrap()["session_id"] = json!(selected.id);
                }
                let result = self.call(call).await?;
                let mut native_result = result.clone();
                if let Some(selected) = &selected {
                    if result["session_id"] != selected.id {
                        return Err(Failure::text("native project session ID changed"));
                    }
                    native_result
                        .as_object_mut()
                        .ok_or(Failure::Generic)?
                        .remove("session_id");
                }
                if !crate::session_projects::result_valid(
                    &validated.operation,
                    &native_result,
                    payload,
                    session,
                ) {
                    return Err(Failure::text(
                        "native project assignment acknowledgement did not match",
                    ));
                }
                if validated.operation == "project_assign" && native_result["status"] == "assigned"
                {
                    self.state.borrow_mut().project_next_poll = 0.0;
                }
                Ok(native_result)
            }
            "hooks_status" => {
                let payload = payload.expect("validated");
                let mut call = Call::new(["hooks", session, "--json"])
                    .payload(payload.clone())
                    .timeout(5.0);
                call.max_output_bytes = Some(16 * 1024);
                let result = self.call(call).await?;
                if !crate::hook_health::result_valid(&result, payload, session) {
                    return Err(Failure::text(
                        "native hook health acknowledgement did not match",
                    ));
                }
                Ok(result)
            }
            "history" => {
                self.history(session, payload.expect("validated"), request_id)
                    .await
            }
            "history_projection" => {
                self.history_projection(session, payload.expect("validated"), request_id)
                    .await
            }
            "catalog" => self.catalog(request_id).await,
            "task_done" => {
                let call = Call::new(["task", "mobile-done", "--json"])
                    .payload(payload.expect("validated").clone())
                    .failure_json();
                task_answer(self.call(call).await?).map_err(|(failure, _)| failure)
            }
            "task_exchange" => {
                // hgs refuses on stdout with a failing exit status; the phone
                // reads why, such as a project another command holds.
                let call = Call::new(["task", "exchange", "--json"])
                    .payload(payload.expect("validated").clone())
                    .timeout(TASK_EXCHANGE_TIMEOUT)
                    .failure_json();
                let result = self.call(call).await?;
                let invalid = || Failure::text("native task exchange answer is invalid");
                match result.get("ok") {
                    Some(Value::Bool(false)) => {
                        let error = result
                            .get("error")
                            .and_then(Value::as_str)
                            .ok_or_else(invalid)?;
                        Err(Failure::text(error))
                    }
                    _ if result.is_object() => Ok(result),
                    _ => Err(invalid()),
                }
            }
            "dirs" => {
                let path = payload
                    .and_then(|payload| payload["path"].as_str())
                    .unwrap_or_default();
                let result = self.call(Call::new(["dirs", path]).timeout(5.0)).await?;
                let valid = result.as_object().is_some_and(|result| {
                    result.get("path").is_some_and(Value::is_string)
                        && result.get("directories").is_some_and(Value::is_array)
                });
                if !valid {
                    return Err(Failure::text("native directory catalog is invalid"));
                }
                let mut kept = Map::new();
                for key in ["path", "parent", "home", "directories", "truncated"] {
                    if let Some(value) = result.get(key) {
                        kept.insert(key.into(), value.clone());
                    }
                }
                kept.insert("request_id".into(), json!(request_id));
                Ok(Value::Object(kept))
            }
            "worktrees" => {
                let path = payload
                    .and_then(|payload| payload["path"].as_str())
                    .unwrap_or_default();
                let raw = self
                    .call(
                        Call::new(["worktrees", "--path", path, "--refresh", "--json"])
                            .timeout(5.0),
                    )
                    .await?;
                // A catalog Python cannot read raises ValueError outside any
                // handler: the generic receipt.
                let mut result = worktree_catalog(&raw, path).map_err(|_| Failure::Generic)?;
                result.insert("request_id".into(), json!(request_id));
                Ok(Value::Object(result))
            }
            _ => Err(Failure::text(validate::UNSUPPORTED)),
        }
    }

    async fn inspect(&self, session: &str, payload: Option<&Value>) -> Result<Value, Failure> {
        let field = |key: &str| payload.and_then(|payload| payload.get(key));
        let archive = field("archive_id").and_then(Value::as_str);
        let mut argv = vec!["inspect".to_owned(), session.to_owned()];
        if let Some(archive) = archive {
            argv.extend(["--archive".into(), archive.into()]);
        }
        let agent = field("agent_id").and_then(Value::as_str);
        if let Some(agent) = agent {
            // Parent membership and immutable identity are checked before the
            // native child inspector enforces its roster.
            let parent = self.call(Call::new(argv.clone())).await?;
            let belongs = parent.as_object().is_some_and(|parent| {
                is_str(parent.get("name"), session)
                    && same(parent.get("run_id"), field("expected_run_id"))
                    && same(
                        parent.get("conversation_id"),
                        field("expected_conversation_id"),
                    )
                    && same(
                        Some(parent.get("archive_id").unwrap_or(&json!(""))),
                        Some(field("archive_id").unwrap_or(&json!(""))),
                    )
                    && parent
                        .get("subagents")
                        .and_then(Value::as_object)
                        .is_some_and(|subagents| subagents.contains_key(agent))
            });
            if !belongs {
                return Err(Failure::text(
                    "subagent does not belong to the exact selected parent",
                ));
            }
            argv.extend(["--agent".into(), agent.into(), "--skip-processes".into()]);
        }
        if let Some(after) = field("after") {
            argv.extend(["--after".into(), int_literal(after).unwrap_or_default()]);
        }
        // Only a main inspection names what a read mark may acknowledge now.
        let current_read = archive.is_none() && agent.is_none() && self.flags.get().current_read;
        if current_read {
            argv.push("--current-read".into());
        }
        let mut result = self.call(Call::new(argv)).await?;
        if self.flags.get().windows_gateway
            && !crate::capabilities::windows_provider(&result["agent"])
        {
            return Err(Failure::text(crate::capabilities::WINDOWS_PROVIDER_REFUSAL));
        }
        let object = result.as_object();
        if let Some(archive) = archive {
            let matches = object.is_some_and(|result| {
                is_str(result.get("archive_id"), archive) && is_str(result.get("name"), session)
            });
            if !matches {
                return Err(Failure::text(
                    "native archive inspection identity did not match",
                ));
            }
        } else {
            let ordinary = object.is_some_and(|result| {
                !is_archived(result)
                    && result
                        .get("name")
                        .is_none_or(|name| name.as_str() == Some(session))
            });
            if !ordinary {
                return Err(Failure::text(
                    "native ordinary inspection returned a different or archived target",
                ));
            }
        }
        let object = object.expect("checked above");
        if let Some(expected) = field("expected_run_id") {
            let conversation = if agent.is_some() {
                object.get("parent_conversation_id")
            } else {
                object.get("conversation_id")
            };
            if !same(object.get("run_id"), Some(expected))
                || !same(conversation, field("expected_conversation_id"))
            {
                return Err(Failure::text(
                    "native inspection run/conversation identity did not match",
                ));
            }
        }
        if let Some(agent) = agent {
            let expected = format!(
                "{}/{agent}",
                field("expected_conversation_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            );
            if !is_str(object.get("agent_id"), agent)
                || !is_str(object.get("conversation_id"), &expected)
            {
                return Err(Failure::text(
                    "native subagent inspection identity did not match",
                ));
            }
        }
        crate::opencode_privacy::row(&mut result);
        let mut result = match result {
            Value::Object(result) => result,
            _ => unreachable!(),
        };
        if !current_read || !crate::inspection::current_read_bound(&result) {
            result.remove("current_read");
        }
        // The device envelope of GET adds request_id: leave room for it
        // below the ordinary one-MiB response bound. A bounded inspection keeps
        // `current_read` only while its newest rows survive: a phone binds it
        // to the newest row it displays.
        let bound = move || {
            let newest = crate::inspection::newest_rows(&result)?;
            let mut bounded = bound_inspection(result, MAX_RESULT - 1024)?;
            if bounded.contains_key("current_read")
                && crate::inspection::newest_rows(&bounded)? != newest
            {
                bounded.remove("current_read");
            }
            Ok(bounded)
        };
        let bounded = tokio::task::spawn_blocking(bound)
            .await
            .unwrap_or(Err(PyError::Type))
            .map_err(py)?;
        Ok(Value::Object(bounded))
    }

    /// `hgs processes` with `--output` or `--stop` and the exact identity.
    pub(crate) async fn processes(
        &self,
        option: &str,
        session: &str,
        payload: &Value,
        request_id: &str,
    ) -> Result<Value, Failure> {
        let text = |key: &str| payload[key].as_str().unwrap_or_default().to_owned();
        let process = text("process_id");
        let mut argv = vec![
            "processes".to_owned(),
            session.into(),
            option.into(),
            process.clone(),
            "--run".into(),
            text("expected_run_id"),
            "--conversation".into(),
            text("expected_conversation_id"),
        ];
        for (field, option) in [("generation", "--generation"), ("archive_id", "--archive")] {
            if payload.get(field).is_some() {
                argv.extend([option.to_owned(), text(field)]);
            }
        }
        let result = self.call(Call::new(argv)).await?;
        let matches = result.as_object().is_some_and(|result| {
            is_str(result.get("id"), &process)
                && same(result.get("run_id"), payload.get("expected_run_id"))
                && same(
                    result.get("conversation_id"),
                    payload.get("expected_conversation_id"),
                )
        });
        if !matches {
            return Err(Failure::text(
                "native process acknowledgement identity did not match",
            ));
        }
        let mut result = match result {
            Value::Object(result) => result,
            _ => unreachable!(),
        };
        result.insert("request_id".into(), json!(request_id));
        result.insert("name".into(), json!(session));
        result.insert("process_id".into(), json!(process));
        if let Some(archive) = payload.get("archive_id") {
            result.insert("archive_id".into(), archive.clone());
        }
        Ok(Value::Object(result))
    }

    /// Full history pages make bounded native indexing progress locally: up
    /// to 31 more reads within 2 s. A one-row metadata probe yields after its
    /// first read so catalog counts do not monopolize the serial native queue.
    async fn history(
        &self,
        session: &str,
        payload: &Value,
        request_id: &str,
    ) -> Result<Value, Failure> {
        let until = self.clock() + 2.0;
        let argv = ["history", session, "--json"];
        let mut result = self
            .call(Call::new(argv).payload(payload.clone()).timeout(2.0))
            .await?;
        let selected = ["before", "after", "around", "around_incoming_seq"]
            .iter()
            .any(|key| payload.get(*key).is_some());
        let metadata_only = payload.get("limit").and_then(Value::as_u64) == Some(1);
        if !selected && !metadata_only {
            for _ in 0..31 {
                if !result
                    .as_object()
                    .is_some_and(|result| result.get("indexing") == Some(&json!(true)))
                {
                    break;
                }
                let remaining = until - self.clock();
                if remaining <= 0.0 {
                    break;
                }
                let previous = progress(&result);
                let following = match self
                    .call(Call::new(argv).payload(payload.clone()).timeout(remaining))
                    .await
                {
                    Ok(following) => following,
                    Err(Failure::Stopped) => return Err(Failure::Stopped),
                    Err(_) => break,
                };
                let following_progress = following.is_object().then(|| progress(&following));
                result = following;
                if following_progress == Some(previous) {
                    break;
                }
            }
        }
        let field = |key: &str| payload.get(key);
        let child = field("agent_id").and_then(Value::as_str);
        let expected_conversation = field("expected_conversation_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let valid = result.as_object().is_some_and(|result| {
            let or_empty = |value: Option<&Value>| value.cloned().unwrap_or(json!(""));
            is_str(result.get("request_id"), request_id)
                && is_str(result.get("name"), session)
                && same(result.get("run_id"), field("expected_run_id"))
                && py_eq(
                    &or_empty(result.get("archive_id")),
                    &or_empty(field("archive_id")),
                )
                && py_eq(
                    &or_empty(result.get("agent_id")),
                    &or_empty(field("agent_id")),
                )
                && is_str(
                    if child.is_some() {
                        result.get("parent_conversation_id")
                    } else {
                        result.get("conversation_id")
                    },
                    expected_conversation,
                )
                && child.is_none_or(|child| {
                    is_str(
                        result.get("conversation_id"),
                        &format!("{expected_conversation}/{child}"),
                    )
                })
                && result.get("events").is_some_and(Value::is_array)
                && result.get("history_epoch").is_some_and(Value::is_string)
                && result.get("head").is_some_and(Value::is_object)
        });
        if !valid {
            return Err(Failure::text(
                "native public history identity or envelope did not match",
            ));
        }
        Ok(result)
    }

    /// Projection reads resolve a fixed range once; they never drive history
    /// indexing or change the client's snapshot boundary.
    async fn history_projection(
        &self,
        session: &str,
        payload: &Value,
        request_id: &str,
    ) -> Result<Value, Failure> {
        let mut call = Call::new(["history-projection", session, "--json"])
            .payload(payload.clone())
            .timeout(2.0);
        call.max_output_bytes = Some(MAX_RESULT);
        let result = self.call(call).await?;
        if !projection_result(&result, payload, session, request_id) {
            return Err(Failure::text(
                "native history projection identity or envelope did not match",
            ));
        }
        Ok(result)
    }

    async fn catalog(&self, request_id: &str) -> Result<Value, Failure> {
        let raw = self.call(Call::new(["account", "ls"]).timeout(5.0)).await?;
        let mut result = crate::accounts::launch_catalog(&raw).map_err(py)?;
        let flags = self.flags.get();
        result["project_launch_supported"] = json!(false);
        result["worktree_supported"] = json!(flags.worktree);
        result["directory_create_supported"] = json!(flags.directory_create);
        if flags.project_launch {
            if let Ok(raw) = self.call(Call::new(["swarm", "get"]).timeout(3.0)).await {
                // A TypeError from a malformed catalog is outside Python's
                // `except (ConnectorError, ValueError)`.
                match launch_projects(&raw) {
                    Ok(Value::Object(projects)) => {
                        for (key, value) in projects {
                            result[key.as_str()] = value;
                        }
                    }
                    Ok(_) | Err(PyError::Value(_)) => {}
                    Err(PyError::Type) => return Err(Failure::Generic),
                }
            }
        }
        result["request_id"] = json!(request_id);
        Ok(result)
    }
}

/// The receipt of a native result: the native status decides the state of
/// a session action, a recovery action and a Terminal input, and a failed or
/// uncertain one carries the native error.
pub fn receipt_of(operation: &str, result: Value) -> Value {
    let by_status = crate::capabilities::LIFECYCLE.contains(&operation)
        || matches!(operation, "terminal_input" | "recovery_action");
    if !by_status {
        return json!({"state": "completed", "result": result, "error": null});
    }
    let status = result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let state = match status.as_str() {
        "submitted" | "scheduled" | "cancelled" => "completed",
        other => other,
    };
    let error = match status.as_str() {
        "failed" | "uncertain" => result.get("error").cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    };
    json!({"state": state, "result": result, "error": error})
}

/// hgs's answer to one of the phone's task commands: the answer, or its
/// refusal and whether that refusal is definite (nothing happened).
fn task_answer(answer: Value) -> Result<Value, (Failure, bool)> {
    let invalid = || (Failure::text("native task answer is invalid"), false);
    match answer.get("ok") {
        Some(Value::Bool(false)) => {
            let error = answer
                .get("error")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?;
            let uncertain = answer.get("uncertain") == Some(&Value::Bool(true));
            Err((Failure::text(error), !uncertain))
        }
        _ if answer.is_object() => Ok(answer),
        _ => Err(invalid()),
    }
}

/// `x != payload[key]` with a string on the payload side.
fn same(value: Option<&Value>, expected: Option<&Value>) -> bool {
    match (value, expected) {
        (Some(value), Some(expected)) => py_eq(value, expected),
        (None, None) => true,
        _ => false,
    }
}

fn projection_integer(value: Option<&Value>) -> Option<i64> {
    int_literal(value?)
        .and_then(|digits| digits.parse::<i64>().ok())
        .filter(|v| *v >= 0)
}

fn projection_text(value: Option<&Value>, maximum: usize) -> bool {
    value.and_then(Value::as_str).is_some_and(|text| {
        !text.is_empty() && text.len() <= maximum && !text.chars().any(char::is_control)
    })
}

fn projection_id(value: Option<&Value>) -> bool {
    value.and_then(Value::as_str).is_some_and(|id| {
        id.len() == 64
            && id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Public page rows retain additive metadata. The transport checks their
/// canonical identity and display body without projecting or rewriting them.
fn projection_row(row: &Value, payload: &Value) -> bool {
    let Some(row) = row.as_object() else {
        return false;
    };
    if !projection_id(row.get("history_id"))
        || !projection_text(row.get("history_cursor"), 8192)
        || !row
            .get("history_stream")
            .and_then(Value::as_str)
            .is_some_and(|stream| ["journal", "provider", "attachment", "voice"].contains(&stream))
        || !row.get("type").and_then(Value::as_str).is_some_and(|kind| {
            [
                "UserPromptSubmit",
                "UserPromptQueued",
                "UserMessage",
                "TurnStarted",
                "QuestionAnswered",
                "AgentMessage",
                "Stop",
            ]
            .contains(&kind)
        })
        || row.contains_key("native_match_hash")
        || !row
            .get("original_ids")
            .and_then(Value::as_array)
            .is_some_and(|ids| {
                ids.len() <= 8
                    && ids
                        .iter()
                        .all(|id| id.as_str().is_some_and(|id| !id.is_empty()))
            })
    {
        return false;
    }
    let agent = payload
        .get("agent_id")
        .and_then(Value::as_str)
        .unwrap_or("main");
    if row.get("agent_id").is_some_and(|value| {
        !value.is_null()
            && !value
                .as_str()
                .is_some_and(|value| value.is_empty() || value == agent)
    }) || row
        .get("incoming_seq")
        .is_some_and(|value| !value.is_null() && projection_integer(Some(value)).is_none())
    {
        return false;
    }
    let attachments = match row.get("attachments") {
        None | Some(Value::Null) => false,
        Some(Value::Array(items)) if items.iter().all(Value::is_object) => !items.is_empty(),
        Some(_) => return false,
    };
    row.get("detail")
        .and_then(Value::as_str)
        .is_some_and(|detail| !detail.trim().is_empty())
        || attachments
}

/// One intact D4 envelope. `complete` covers the requested range, including
/// unresolved entries; it is never an acknowledgement of a display prefix.
fn projection_result(result: &Value, payload: &Value, session: &str, request_id: &str) -> bool {
    if json::size(result, 880 * 1024).is_err() {
        return false;
    }
    let Some(result) = result.as_object() else {
        return false;
    };
    let empty = json!("");
    let child = payload.get("agent_id").and_then(Value::as_str);
    let conversation = payload["expected_conversation_id"]
        .as_str()
        .unwrap_or_default();
    let expected = child.map_or_else(
        || conversation.to_owned(),
        |id| format!("{conversation}/{id}"),
    );
    let until = projection_integer(payload.get("projection_until")).expect("validated");
    let mut previous = projection_integer(payload.get("projection_after")).expect("validated");
    if !is_str(result.get("request_id"), request_id)
        || !is_str(result.get("name"), session)
        || !same(result.get("run_id"), payload.get("expected_run_id"))
        || result.get("archive_id").unwrap_or(&empty) != payload.get("archive_id").unwrap_or(&empty)
        || result.get("agent_id").unwrap_or(&empty) != payload.get("agent_id").unwrap_or(&empty)
        || !is_str(result.get("conversation_id"), &expected)
        || !is_str(
            Some(result.get("parent_conversation_id").unwrap_or(&empty)),
            child.map_or("", |_| conversation),
        )
        || !same(result.get("history_epoch"), payload.get("history_epoch"))
        || projection_integer(result.get("projection_generation"))
            .is_none_or(|generation| generation < until)
        || !result.get("complete").is_some_and(Value::is_boolean)
    {
        return false;
    }
    let Some(entries) = result
        .get("projection_changes")
        .and_then(Value::as_array)
        .filter(|v| v.len() <= 256)
    else {
        return false;
    };
    for entry in entries {
        let Some(entry) = entry.as_object() else {
            return false;
        };
        let fields = [
            "generation",
            "history_id",
            "change",
            "resolution",
            "successor",
        ];
        let Some(generation) = projection_integer(entry.get("generation")) else {
            return false;
        };
        let change = entry
            .get("change")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !fields.iter().all(|key| entry.contains_key(*key))
            || !entry
                .keys()
                .all(|key| fields.contains(&key.as_str()) || key == "reason")
            || !(previous < generation && generation <= until)
            || !projection_id(entry.get("history_id"))
            || !["retired", "refreshed"].contains(&change)
        {
            return false;
        }
        previous = generation;
        let successor = &entry["successor"];
        let valid = match entry.get("resolution").and_then(Value::as_str) {
            Some("unresolved") => {
                successor.is_null()
                    && entry.get("reason").is_none_or(|reason| {
                        reason.as_str().is_some_and(|reason| {
                            ["no_successor", "unexpected_successor", "oversize"].contains(&reason)
                        })
                    })
            }
            Some("resolved") if !entry.contains_key("reason") => {
                if change == "retired" {
                    successor.is_null()
                } else {
                    projection_row(successor, payload)
                }
            }
            _ => false,
        };
        if !valid {
            return false;
        }
    }
    let next = projection_integer(result.get("next_projection_after"));
    if result["complete"] == json!(true) {
        next == Some(until)
    } else {
        !entries.is_empty() && previous < until && next == Some(previous)
    }
}

/// `json.dumps([source_status, head, events], sort_keys=True)`: equal texts
/// mean the head made no progress.
fn progress(result: &Value) -> Option<String> {
    let key = json!([
        result.get("source_status").cloned().unwrap_or(Value::Null),
        result.get("head").cloned().unwrap_or(Value::Null),
        result.get("events").cloned().unwrap_or(Value::Null),
    ]);
    json::canonical(&key).ok()
}

/// `context.mark_read_result(result, payload)`.
pub fn mark_read_result(result: &Value, payload: &Value) -> bool {
    if py_eq(result, &json!({"ok": false, "error": "not_found"})) {
        return true;
    }
    let Some(result) = result
        .as_object()
        .filter(|result| result.get("ok") == Some(&json!(true)))
    else {
        return false;
    };
    let applied = result
        .get("applied")
        .and_then(Value::as_object)
        .is_some_and(|applied| {
            applied.len() == 2
                && ["reply_id", "attention_signature"]
                    .iter()
                    .all(|key| applied.get(*key).is_some_and(Value::is_boolean))
        });
    let read = match result.get("read") {
        None | Some(Value::Null) => true,
        Some(Value::Object(read)) => {
            same(read.get("conversation_id"), payload.get("conversation_id"))
        }
        Some(_) => false,
    };
    applied && read
}

/// `worktrees.absolute(value)`.
pub(crate) fn absolute(value: Option<&Value>) -> Result<String, PyError> {
    let value = bounded(value, 4096, false)?;
    if !value.starts_with('/') {
        return Err(PyError::value("worktree path must be absolute"));
    }
    Ok(value)
}

/// `worktrees.catalog(raw, requested)`.
pub fn worktree_catalog(raw: &Value, requested: &str) -> Result<Map<String, Value>, PyError> {
    let rows = match raw.get("worktrees") {
        Some(Value::Array(rows)) if raw.is_object() && rows.len() <= 512 => rows,
        _ => return Err(PyError::value("invalid native worktree catalog")),
    };
    let state = bounded(raw.get("state"), 32, false)?;
    let mut result = Map::new();
    result.insert("state".into(), json!(state));
    result.insert("path".into(), json!(absolute(raw.get("path"))?));
    result.insert("requested_path".into(), json!(requested));
    result.insert(
        "stale".into(),
        json!(raw.get("stale") == Some(&json!(true))),
    );
    result.insert(
        "partial".into(),
        json!(raw.get("partial") == Some(&json!(true))),
    );
    for key in ["common_dir", "selected_root"] {
        if raw.get(key).is_some_and(|value| !value.is_null()) {
            result.insert(key.into(), json!(absolute(raw.get(key))?));
        }
    }
    let identity = result
        .get("common_dir")
        .is_some_and(|value| value.as_str().is_some_and(|text| !text.is_empty()));
    if state == "ok" && !identity {
        return Err(PyError::value("worktree repository identity is missing"));
    }
    let mut worktrees = Vec::new();
    for row in rows {
        let kind = row.get("kind").and_then(Value::as_str);
        let Some(kind) = kind.filter(|kind| ["main", "linked", "bare"].contains(kind)) else {
            return Err(PyError::value("invalid native worktree entry"));
        };
        let mut item = Map::new();
        item.insert("path".into(), json!(absolute(row.get("path"))?));
        item.insert("kind".into(), json!(kind));
        item.insert(
            "available".into(),
            json!(row.get("available") == Some(&json!(true))),
        );
        for key in ["branch", "head"] {
            if row.get(key).is_some_and(crate::pyvalue::truthy) {
                item.insert(key.into(), json!(bounded(row.get(key), 256, false)?));
            }
        }
        for key in ["detached", "locked", "prunable"] {
            item.insert(key.into(), json!(row.get(key) == Some(&json!(true))));
        }
        worktrees.push(Value::Object(item));
    }
    result.insert("worktrees".into(), Value::Array(worktrees));
    Ok(result)
}
