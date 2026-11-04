use super::audit::help::Image;
use crate::desktop_notification_controls::{reply_matches, Schedule, Source, Subscription};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, PoisonError, Weak,
    },
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use zerus_client_core::{
    desktop::{
        drain::{Charge, Closed, Gate, Role},
        notification_control::{
            self as wire, Command, FocusState, Preferences, Query, Receipt, Scope, Status,
        },
        relay,
        ui_requests::{Registration, Request},
        Responder,
    },
    run::{run_owned, OwnedError},
    settings::Settings,
    Invocation,
};

pub(crate) const UNAVAILABLE: &str =
    zerus_client_core::source!("Notification settings are unavailable from the desktop service.");
pub(crate) const UNCONFIRMED: &str = zerus_client_core::source!("The previous notification setting is unconfirmed. Check its result before making another change.");
const KEY: &str = "notifications/optionalQuestions";
const WAIT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub(crate) struct Setup {
    pub source: Source,
    pub settings: Settings,
}
#[derive(Clone, Default, Serialize)]
pub(crate) struct View {
    pub status: Option<Status>,
    pub pending: bool,
    pub request_id: Option<String>,
    pub running: bool,
    pub available: bool,
    pub code: String,
}
enum Phase {
    Queued,
    Unknown,
}
struct Pending {
    command: Command,
    configuration: String,
    phase: Phase,
    exit_confirmed: bool,
}
#[derive(Default)]
struct State {
    current: Weak<Session>,
    status: Option<Status>,
    pending: Option<Pending>,
    status_needed: bool,
    check_needed: bool,
    running: bool,
    initialized: bool,
    code: String,
}
pub(crate) struct Controller {
    setup: Option<Setup>,
    state: Mutex<State>,
    gate: Gate,
    changed: Notify,
    publish: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
pub(crate) struct Session {
    scope: Scope,
    relay: Arc<relay::Connection>,
    responder: Arc<Responder>,
    native: wire::native::Connection,
    image: Arc<Image>,
    stop: Arc<AtomicBool>,
    subscription: Mutex<Option<Subscription>>,
}
impl Session {
    fn invalidate(&self) {
        let _ = self.native.observe(FocusState::Unknown);
        self.native.invalidate();
        self.subscription
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.invalidate();
    }
}
impl Controller {
    pub(crate) fn enabled(&self) -> bool {
        self.setup.is_some()
    }
    pub(crate) fn new(setup: Option<Setup>) -> Arc<Self> {
        Arc::new(Self {
            setup,
            state: Mutex::new(State::default()),
            gate: Gate::new(1).unwrap(),
            changed: Notify::new(),
            publish: Mutex::new(None),
        })
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
    pub(crate) fn hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.publish.lock().unwrap_or_else(PoisonError::into_inner) = Some(hook);
    }
    fn publish(&self) {
        self.changed.notify_one();
        if self.setup.is_none() || !self.gate.snapshot().is_ok_and(|v| !v.closed) {
            return;
        }
        let hook = self
            .publish
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }
    pub(crate) fn view(&self) -> Option<View> {
        self.setup.as_ref()?;
        let state = self.state();
        Some(View {
            status: state.status.clone(),
            pending: state.pending.is_some(),
            request_id: state
                .pending
                .as_ref()
                .map(|v| v.command.request_id().into()),
            running: state.running,
            available: state
                .current
                .upgrade()
                .is_some_and(|s| !s.stop.load(Ordering::Acquire))
                && state.status.is_some(),
            code: state.code.clone(),
        })
    }
    pub(crate) async fn connect(
        self: &Arc<Self>,
        scope: Scope,
        relay: Arc<relay::Connection>,
        responder: Option<Arc<Responder>>,
        stop: Arc<AtomicBool>,
        supported: bool,
    ) -> Option<Arc<Session>> {
        let setup = self.setup.as_ref()?;
        if !supported {
            self.disconnect();
            return None;
        }
        // Capture before this connection's successful marker probe. Retain the
        // open original and compare it before spawn AND after every child exit.
        let charge = loop {
            let changed = self.changed.notified();
            if stop.load(Ordering::Acquire) {
                return None;
            }
            match self.gate.admit(Role::Fleet) {
                Ok(charge) => break charge,
                Err(zerus_client_core::desktop::drain::Refused::Capacity) => {
                    // The previous connection's bounded call may have returned
                    // while an original pipe is still retained. Wait for real
                    // custody, without admitting a second helper or losing the
                    // current connection's eventual fresh Unknown observation.
                    tokio::select! { _ = changed => (), _ = tokio::time::sleep(Duration::from_millis(250)) => () }
                }
                Err(_) => return None,
            }
        };
        let selected = relay.clone();
        let cancelled = stop.clone();
        let image = tauri::async_runtime::spawn_blocking(move || {
            let path = selected.executable().ok()?;
            let image = Arc::new(Image::capture(path)?);
            let held: Arc<dyn Send + Sync> = Arc::new((charge, image.clone(), selected.clone()));
            let inv = Invocation {
                args: vec!["--help".into()],
                stdin: Some(vec![]),
                timeout: WAIT,
                request_id: None,
            };
            let bytes =
                run_owned(path, &inv, &[], &cancelled, |command| command.spawn(), held).ok()?;
            let text = std::str::from_utf8(&bytes).ok()?;
            if bytes.len() > 64 << 10
                || !image.current(selected.executable().ok()?)
                || ![
                    wire::HELP_MARKER,
                    zerus_client_core::desktop::ui_requests::HELP_MARKER,
                ]
                .iter()
                .all(|m| text.lines().any(|line| line.trim() == *m))
            {
                return None;
            }
            Some(image)
        })
        .await
        .ok()
        .flatten();
        let Some(image) = image else {
            self.disconnect();
            return None;
        };
        let session = (|| {
            if !supported
                || stop.load(Ordering::Acquire)
                || !self.gate.snapshot().is_ok_and(|v| !v.closed)
            {
                return None;
            }
            let responder = responder?;
            let native = responder.notification_connect(scope.clone()).ok()?;
            Some(Arc::new(Session {
                scope,
                relay,
                responder,
                native,
                image,
                stop,
                subscription: Mutex::new(None),
            }))
        })();
        let Some(session) = session else {
            self.disconnect();
            return None;
        };
        {
            let mut state = self.state();
            state.current = Arc::downgrade(&session);
            state.status = None;
            state.status_needed = true;
            state.check_needed = state.pending.is_some();
            state.initialized = false;
            state.running = false;
            state.code.clear();
        }
        let weak = Arc::downgrade(&session);
        let owner = Arc::downgrade(self);
        let subscription = setup.source.subscribe(
            session.native.id().into(),
            Arc::new(move |focus| {
                if let Some(session) = weak.upgrade() {
                    if !session.stop.load(Ordering::Acquire) {
                        let _ = session.native.observe(focus);
                    }
                }
                if let Some(owner) = owner.upgrade() {
                    owner.changed.notify_one();
                }
            }),
        );
        *session
            .subscription
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(subscription);
        let _ = setup.source.refresh();
        self.publish();
        Some(session)
    }
    pub(crate) fn disconnect(&self) {
        self.invalidate();
        self.publish();
    }
    fn invalidate(&self) {
        let old = {
            let mut state = self.state();
            let old = state.current.upgrade();
            state.current = Weak::new();
            state.running = false;
            // A queued edit has no original child and has never been sent.
            // An uncertain sent edit survives reconnect for lookup only.
            if state
                .pending
                .as_ref()
                .is_some_and(|v| matches!(v.phase, Phase::Queued))
            {
                state.pending = None;
            }
            state.code = UNAVAILABLE.into();
            old
        };
        if let Some(old) = old {
            old.invalidate();
        }
        self.changed.notify_one();
    }
    pub(crate) fn close(&self) -> Closed {
        // Stop is synchronous and must not enter a potentially held webview
        // publication callback. Evidence invalidation and custody remain real.
        self.invalidate();
        self.gate.close()
    }
    pub(crate) fn edit(&self, enabled: bool) -> Result<View, String> {
        {
            let mut state = self.state();
            let session = state
                .current
                .upgrade()
                .filter(|s| !s.stop.load(Ordering::Acquire))
                .ok_or(UNAVAILABLE)?;
            if state.pending.is_some() {
                return Err(UNCONFIRMED.into());
            }
            let status = state
                .status
                .as_ref()
                .filter(|s| s.scope == session.scope)
                .ok_or(UNAVAILABLE)?;
            let command = Command::Configure {
                request_id: uuid::Uuid::new_v4().to_string(),
                journal: status.journal.clone(),
                revision: status.revision,
                preferences: Preferences {
                    optional_questions: enabled,
                },
            };
            state.pending = Some(Pending {
                command,
                configuration: session.scope.configuration.clone(),
                phase: Phase::Queued,
                exit_confirmed: false,
            });
            state.code.clear();
        }
        self.publish();
        self.view().ok_or_else(|| UNAVAILABLE.into())
    }
    pub(crate) fn check(&self) -> Result<View, String> {
        {
            let mut state = self.state();
            state.status_needed = true;
            state.check_needed = true;
        }
        self.publish();
        self.view().ok_or_else(|| UNAVAILABLE.into())
    }
    fn current(&self, session: &Arc<Session>) -> bool {
        !session.stop.load(Ordering::Acquire)
            && self
                .state()
                .current
                .upgrade()
                .is_some_and(|s| Arc::ptr_eq(&s, session))
    }
    fn next(&self, session: &Arc<Session>) -> Option<Work> {
        let mut state = self.state();
        if !state
            .current
            .upgrade()
            .is_some_and(|s| Arc::ptr_eq(&s, session))
        {
            return None;
        }
        if let Some(pending) = &state.pending {
            if pending.configuration != session.scope.configuration {
                state.code = UNCONFIRMED.into();
                if std::mem::take(&mut state.status_needed) {
                    state.running = true;
                    return Some(Work::Status);
                }
                return None;
            }
            if matches!(pending.phase, Phase::Queued) {
                let command = pending.command.clone();
                state.pending.as_mut().unwrap().phase = Phase::Unknown;
                state.running = true;
                return Some(Work::Configure(command));
            }
            if state.check_needed {
                let command = pending.command.clone();
                state.check_needed = false;
                state.running = true;
                return Some(Work::Lookup(command));
            }
        }
        if std::mem::take(&mut state.status_needed) {
            state.running = true;
            return Some(Work::Status);
        }
        None
    }
    fn has_work(&self, session: &Arc<Session>) -> bool {
        let state = self.state();
        if !state
            .current
            .upgrade()
            .is_some_and(|s| Arc::ptr_eq(&s, session))
        {
            return false;
        }
        state.status_needed
            || state.pending.as_ref().is_some_and(|pending| {
                pending.configuration == session.scope.configuration
                    && (matches!(pending.phase, Phase::Queued) || state.check_needed)
            })
    }
    fn status(&self, session: &Arc<Session>, status: Status) {
        let mut state = self.state();
        if session.stop.load(Ordering::Acquire)
            || !state
                .current
                .upgrade()
                .is_some_and(|s| Arc::ptr_eq(&s, session))
        {
            return;
        }
        if !status.valid() || status.scope != session.scope {
            state.status = None;
            state.code = UNAVAILABLE.into();
            return;
        }
        if let Some(setup) = &self.setup {
            if status.configured && state.pending.is_none() {
                // A fresh current status mirrors the existing local preference;
                // a historical mutation receipt never overwrites current state.
                let _ = setup
                    .settings
                    .set(KEY, json!(status.preferences.optional_questions));
            } else if !status.configured && state.pending.is_none() && !state.initialized {
                state.initialized = true;
                state.pending = Some(Pending {
                    command: Command::Configure {
                        request_id: uuid::Uuid::new_v4().to_string(),
                        journal: status.journal.clone(),
                        revision: status.revision,
                        preferences: Preferences {
                            optional_questions: setup.settings.flag(KEY),
                        },
                    },
                    configuration: session.scope.configuration.clone(),
                    phase: Phase::Queued,
                    exit_confirmed: false,
                });
            }
        }
        state.status = Some(status);
        if state.code == UNAVAILABLE {
            state.code.clear();
        }
    }
    fn receipt(&self, command: &Command, configuration: &str, receipt: Receipt) {
        if !receipt.matches(command, configuration) {
            return;
        }
        let mut state = self.state();
        if !state
            .pending
            .as_ref()
            .is_some_and(|v| v.command == *command && v.configuration == configuration)
        {
            return;
        }
        // Receipt epoch is historical. Always refresh the CURRENT scoped value.
        state.pending = None;
        state.status_needed = true;
        state.code = if matches!(receipt.outcome, wire::Outcome::Saved) {
            String::new()
        } else {
            "The previous change was not applied.".into()
        };
    }
}
enum Work {
    Status,
    Configure(Command),
    Lookup(Command),
}
struct Call {
    bytes: Result<Vec<u8>, ()>,
    spawned: bool,
    exit_confirmed: bool,
}
struct Resources {
    _charge: Charge,
    _registration: Option<Arc<dyn Send + Sync>>,
    _session: Arc<Session>,
    wake: Weak<Controller>,
}
impl Drop for Resources {
    fn drop(&mut self) {
        if let Some(owner) = self.wake.upgrade() {
            owner.changed.notify_one();
        }
    }
}
fn call(
    owner: &Arc<Controller>,
    session: &Arc<Session>,
    charge: Charge,
    request: Option<Request>,
    input: Value,
) -> (Call, Option<wire::NativeFocus>) {
    let mut attached = None;
    let registration = match request {
        Some(request) => match session
            .responder
            .prepare_ui_request(uuid::Uuid::new_v4().to_string(), request)
        {
            Ok(registration) => {
                attached = registration
                    .notification_evidence()
                    .and_then(|v| v.focus.clone());
                Some(registration)
            }
            Err(_) => {
                return (
                    Call {
                        bytes: Err(()),
                        spawned: false,
                        exit_confirmed: false,
                    },
                    None,
                )
            }
        },
        None => None,
    };
    let payload = match &registration {
        Some(r) => serde_json::to_vec(r.envelope()),
        None => serde_json::to_vec(&input),
    };
    let Ok(input) = payload else {
        return (
            Call {
                bytes: Err(()),
                spawned: false,
                exit_confirmed: false,
            },
            None,
        );
    };
    if input.len() > zerus_watch_protocol::CONTROL_LINE_BYTES {
        return (
            Call {
                bytes: Err(()),
                spawned: false,
                exit_confirmed: false,
            },
            None,
        );
    }
    let Ok(helper) = session.relay.executable() else {
        return (
            Call {
                bytes: Err(()),
                spawned: false,
                exit_confirmed: false,
            },
            None,
        );
    };
    if !session.image.current(helper) {
        return (
            Call {
                bytes: Err(()),
                spawned: false,
                exit_confirmed: false,
            },
            None,
        );
    }
    let inv = Invocation {
        args: vec![
            "desktop".into(),
            if registration.is_some() {
                "frontend-request"
            } else {
                "request"
            }
            .into(),
            "--json".into(),
        ],
        stdin: Some(input),
        timeout: WAIT,
        request_id: None,
    };
    let held: Arc<dyn Send + Sync> = Arc::new(Resources {
        _charge: charge,
        _registration: registration.as_ref().map(Registration::resources),
        _session: session.clone(),
        wake: Arc::downgrade(owner),
    });
    let spawned = Arc::new(AtomicBool::new(false));
    let started = spawned.clone();
    let result = run_owned(
        helper,
        &inv,
        &[],
        &session.stop,
        move |command| {
            let child = command.spawn()?;
            started.store(true, Ordering::Release);
            if let Some(registration) = registration {
                let _ = registration.bind_original(&child);
            }
            Ok(child)
        },
        held,
    );
    let spawned = spawned.load(Ordering::Acquire);
    let exit_confirmed = spawned && !matches!(result, Err(OwnedError::ExitUnverified));
    let bytes = result.map_err(|_| ()).and_then(|bytes| {
        if bytes.len() <= wire::BYTES
            && session
                .relay
                .executable()
                .is_ok_and(|path| session.image.current(path))
        {
            Ok(bytes)
        } else {
            Err(())
        }
    });
    (
        Call {
            bytes,
            spawned,
            exit_confirmed,
        },
        attached,
    )
}

async fn due_focus(
    owner: &Controller,
    session: &Arc<Session>,
    schedule: &Schedule,
) -> Option<wire::NativeFocus> {
    let focus = session.native.snapshot()?;
    if !schedule.due(&focus, Instant::now()) {
        return None;
    }
    if focus.state == FocusState::Foreground {
        if owner.has_work(session) {
            return None;
        }
        let refresh = owner.setup.as_ref()?.source.refresh();
        let observed = if let Some(refresh) = refresh {
            let wait = refresh.wait();
            tokio::pin!(wait);
            loop {
                tokio::select! {
                    complete = &mut wait => break complete,
                    _ = owner.changed.notified() => (),
                    _ = tokio::time::sleep(Duration::from_millis(250)) => (),
                }
                if !owner.current(session) || owner.has_work(session) {
                    return None;
                }
                // Native loss invalidates Foreground immediately, even while
                // a queued enumeration has not yet run on the main thread.
                if session.native.snapshot()?.state != FocusState::Foreground {
                    break true;
                }
            }
        } else {
            false
        };
        if !observed {
            let _ = session.native.observe(FocusState::Unknown);
        }
    }
    if !owner.current(session) {
        return None;
    }
    session
        .native
        .snapshot()
        .filter(|v| schedule.due(v, Instant::now()))
}

pub(crate) async fn run(owner: Arc<Controller>, session: Arc<Session>) {
    let mut schedule = Schedule::default();
    loop {
        let changed = owner.changed.notified();
        if !owner.current(&session) {
            break;
        }
        let focus = due_focus(&owner, &session, &schedule).await;
        if focus.is_none() && !owner.has_work(&session) {
            tokio::select! {_ = changed=>(),_ = tokio::time::sleep(Duration::from_millis(wire::FOCUS_REFRESH_MS))=>()};
            continue;
        }
        let charge = match owner.gate.admit(Role::Fleet) {
            Ok(charge) => charge,
            Err(_) => {
                tokio::select! {_ = changed=>(),_ = tokio::time::sleep(Duration::from_millis(250))=>()};
                continue;
            }
        };
        let work = if focus.is_some() {
            None
        } else {
            owner.next(&session)
        };
        if focus.is_none() && work.is_none() {
            drop(charge);
            continue;
        }
        let (request, input) = if focus.is_some() {
            let control = Command::Focus {
                request_id: uuid::Uuid::new_v4().to_string(),
            };
            (
                Some(Request::NotificationControl {
                    scope: session.scope.clone(),
                    control,
                }),
                Value::Null,
            )
        } else {
            match work.as_ref().unwrap() {
                Work::Status => (
                    None,
                    json!({"op":"notification_preferences_status","request_id":uuid::Uuid::new_v4().to_string(),"scope":session.scope}),
                ),
                Work::Configure(control) => (
                    Some(Request::NotificationControl {
                        scope: session.scope.clone(),
                        control: control.clone(),
                    }),
                    Value::Null,
                ),
                Work::Lookup(original) => (
                    None,
                    json!({"op":"notification_preferences_result","query":Query {request_id:uuid::Uuid::new_v4().to_string(),scope:session.scope.clone(),original:original.clone()}}),
                ),
            }
        };
        let original = request.clone();
        let original_input = input.clone();
        let (state, selected) = (owner.clone(), session.clone());
        let result = tauri::async_runtime::spawn_blocking(move || {
            call(&state, &selected, charge, request, input)
        })
        .await;
        let (result, attached) = result.unwrap_or((
            Call {
                bytes: Err(()),
                spawned: true,
                exit_confirmed: false,
            },
            None,
        ));
        if let Some(Request::NotificationControl {
            control: command @ Command::Focus { .. },
            ..
        }) = original
        {
            if let Some(attached) = attached {
                schedule.attempted(attached.clone(), Instant::now());
                let matched = result
                    .bytes
                    .ok()
                    .and_then(|v| serde_json::from_slice::<wire::FocusReply>(&v).ok())
                    .is_some_and(|reply| {
                        reply_matches(&reply, &command, &session.scope, &attached)
                    });
                if !matched {
                    schedule.lost_reply();
                }
            } else if let Some(focus) = focus {
                schedule.attempted(focus, Instant::now());
                schedule.lost_reply();
            }
            continue;
        }
        let Some(work) = work else {
            continue;
        };
        match work {
            Work::Status => match result
                .bytes
                .ok()
                .and_then(|v| serde_json::from_slice::<Status>(&v).ok())
            {
                Some(status) => owner.status(&session, status),
                None => {
                    let mut state = owner.state();
                    state.status = None;
                    state.code = UNAVAILABLE.into();
                }
            },
            Work::Configure(command) => {
                {
                    let mut state = owner.state();
                    if let Some(pending) = state.pending.as_mut().filter(|v| v.command == command) {
                        pending.exit_confirmed = result.exit_confirmed;
                        if !result.spawned {
                            state.pending = None;
                            state.code = UNAVAILABLE.into();
                        }
                    }
                }
                if let Some(receipt) = result
                    .bytes
                    .ok()
                    .and_then(|v| serde_json::from_slice::<Receipt>(&v).ok())
                {
                    owner.receipt(&command, &session.scope.configuration, receipt);
                }
            }
            Work::Lookup(command) => {
                let query = serde_json::from_value::<Query>(original_input["query"].clone()).ok();
                let lookup = result
                    .bytes
                    .ok()
                    .and_then(|v| serde_json::from_slice::<wire::Lookup>(&v).ok());
                if let (Some(query), Some(lookup)) = (query, lookup) {
                    if lookup.matches(&query) {
                        if let Some(receipt) = lookup.receipt {
                            owner.receipt(&command, &session.scope.configuration, receipt);
                        } else if lookup.definitive_not_applied {
                            let mut state = owner.state();
                            if state
                                .pending
                                .as_ref()
                                .is_some_and(|v| v.command == command && v.exit_confirmed)
                            {
                                state.pending = None;
                                state.status_needed = true;
                                state.code = "The previous change was not applied.".into();
                            }
                        }
                    }
                }
            }
        }
        owner.state().running = false;
        owner.publish();
    }
    schedule.disconnected();
    session.invalidate();
}

#[tauri::command]
pub(crate) fn desktop_notification_preferences_set(
    source: tauri::State<'_, crate::commands::AppState>,
    enabled: bool,
) -> Result<View, String> {
    let service = source.desktop_service().ok_or(UNAVAILABLE)?;
    service.shared.notification_controls.edit(enabled)
}
#[tauri::command]
pub(crate) fn desktop_notification_preferences_check(
    source: tauri::State<'_, crate::commands::AppState>,
) -> Result<View, String> {
    let service = source.desktop_service().ok_or(UNAVAILABLE)?;
    service.shared.notification_controls.check()
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
