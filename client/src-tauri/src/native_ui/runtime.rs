//! Interactive native workspace controller. It reuses the existing CLI; it
//! never changes a bridge, owns agents, or persists a launch credential.
use super::Launch;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use zerus_client_core::{Exit, Invocation, Target, ViewRow};

mod renderer;

pub(super) const CONNECTING: &str = "Connecting to native DeepSeek UI…";
#[cfg(target_os = "linux")]
pub(super) const LOADING: &str = "Loading native DeepSeek UI…";
pub(super) const OFFLINE: &str = "Native UI is unavailable while this machine is offline.";
pub(super) const FAILED: &str =
    "Native UI could not load. Use Reload to reconnect, or open it in your browser.";
#[cfg(target_os = "linux")]
pub(super) const STOPPED: &str = "The native UI renderer stopped. Use Reload to reopen it.";
pub(super) const BROWSER: &str = "The embedded native UI is not installed. Open it in your browser with Open native DeepSeek UI in the session menu.";
const CHANGED: &str = "The session state changed. Open its actions again.";
const INVALID: &str = "Invalid native UI connection response";
const INVALID_ADDRESS: &str = "The native UI returned an invalid connection address.";
const OPEN_FAILED: &str = "Could not open native DeepSeek UI.";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct Request {
    pub nonce: String,
    pub catalog_key: String,
    pub target: Target,
}

impl Request {
    fn valid(&self) -> bool {
        uuid::Uuid::parse_str(&self.nonce).is_ok_and(|id| id.hyphenated().to_string() == self.nonce)
            && (self.target.computer.is_empty()
                || zerus_client_core::fleet::valid_alias(&self.target.computer))
            && [
                &self.catalog_key,
                &self.target.name,
                &self.target.run_id,
                &self.target.conversation_id,
            ]
            .iter()
            .all(|value| {
                !value.is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
            })
    }

    fn matches(&self, row: Option<&ViewRow>) -> bool {
        row.is_some_and(|row| {
            row.name == self.target.name
                && row.run_id.as_deref() == Some(&self.target.run_id)
                && row.conversation_id.as_deref() == Some(&self.target.conversation_id)
                && row.notify.cmd == "dsh"
                && row.state != "archived"
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    #[default]
    Connecting,
    #[cfg(target_os = "linux")]
    Loading,
    Ready,
    Browser,
    Failed,
    Stopped,
    Offline,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Status {
    pub request: Request,
    pub revision: u64,
    pub status: Phase,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Bounds {
    pub nonce: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Bounds {
    pub(super) fn valid(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|n| n.is_finite())
            && self.x >= 0.0
            && self.y >= 0.0
            && self.width > 0.0
            && self.height > 0.0
            && self.x + self.width <= 32768.0
            && self.y + self.height <= 32768.0
    }
}

#[derive(Default)]
struct Inner {
    pub generation: u64,
    pub request: Option<Request>,
    pub cancel: Option<Arc<AtomicBool>>,
    pub revision: u64,
    pub phase: Phase,
    pub text: String,
    pub views: Vec<renderer::View>,
    pub active: Option<String>,
    pub bounds: Option<Bounds>,
    pub visible: bool,
    pub dialog: bool,
    #[cfg(target_os = "linux")]
    pub serial: u64,
}

#[derive(Clone, Default)]
pub(crate) struct NativeUi {
    inner: Arc<Mutex<Inner>>,
    busy: Arc<AtomicBool>,
    #[cfg(target_os = "linux")]
    link_busy: Arc<AtomicBool>,
}

pub(super) fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|error| error.into_inner())
}

impl NativeUi {
    pub(crate) fn shutdown(&self) {
        let views = {
            let mut inner = lock(&self.inner);
            inner.generation = inner.generation.saturating_add(1);
            if let Some(cancel) = inner.cancel.take() {
                cancel.store(true, Ordering::Relaxed);
            }
            inner.request = None;
            inner.active = None;
            inner.visible = false;
            std::mem::take(&mut inner.views)
        };
        for view in views {
            let _ = view.webview.close();
        }
    }
    pub(super) fn current(&self, request: &Request, generation: u64) -> bool {
        let inner = lock(&self.inner);
        inner.generation == generation && inner.request.as_ref() == Some(request)
    }

    pub(super) fn status(&self) -> Option<Status> {
        let inner = lock(&self.inner);
        Some(Status {
            request: inner.request.clone()?,
            revision: inner.revision,
            status: inner.phase,
            text: inner.text.clone(),
        })
    }

    pub(super) fn publish_for(
        &self,
        app: &AppHandle,
        request: &Request,
        generation: u64,
        phase: Phase,
        text: &str,
    ) {
        let status = {
            let mut inner = lock(&self.inner);
            if inner.generation != generation || inner.request.as_ref() != Some(request) {
                return;
            }
            if inner.phase == phase && inner.text == text {
                return;
            }
            inner.phase = phase;
            inner.text = text.into();
            inner.revision = inner.revision.saturating_add(1);
            Status {
                request: request.clone(),
                revision: inner.revision,
                status: phase,
                text: text.into(),
            }
        };
        let _ = app.emit("native-ui", status);
    }
}

struct Busy(Arc<AtomicBool>);
impl Drop for Busy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(super) async fn on_main<T: Send + 'static>(
    app: &AppHandle,
    action: impl FnOnce(&AppHandle) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (send, receive) = tokio::sync::oneshot::channel();
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let _ = send.send(action(&handle));
    })
    .map_err(|_| OPEN_FAILED.to_owned())?;
    receive.await.map_err(|_| OPEN_FAILED.to_owned())?
}

fn invocation(request: &Request, browser: bool) -> Invocation {
    let mut args = Vec::new();
    if !request.target.computer.is_empty() {
        args.push(format!("@{}", request.target.computer));
    }
    args.extend(["native-ui".into(), request.target.name.clone()]);
    if !browser {
        args.push("--json".into());
    }
    Invocation {
        args,
        stdin: None,
        timeout: Duration::from_secs(if request.target.computer.is_empty() {
            45
        } else {
            60
        }),
        request_id: None,
    }
}

fn parse(exit: Exit) -> Result<Launch, String> {
    let Exit::Code {
        code: 0, stdout, ..
    } = exit
    else {
        return Err(OPEN_FAILED.into());
    };
    let reply: serde_json::Value = serde_json::from_slice(&stdout).map_err(|_| INVALID)?;
    let address = reply
        .get("url")
        .and_then(serde_json::Value::as_str)
        .ok_or(INVALID)?;
    Launch::parse(address).ok_or_else(|| INVALID_ADDRESS.into())
}

fn available(pump: &crate::pump::Pump, request: &Request) -> Result<(), String> {
    if !request.valid()
        || !request.matches(
            pump.row(&request.target.computer, &request.catalog_key)
                .as_ref(),
        )
    {
        return Err(CHANGED.into());
    }
    if !pump.online(&request.target.computer) {
        return Err(OFFLINE.into());
    }
    Ok(())
}

async fn read(
    hgs: &Path,
    request: &Request,
    cancel: Arc<AtomicBool>,
    browser: bool,
) -> Result<Exit, String> {
    let hgs = hgs.to_owned();
    let call = invocation(request, browser);
    tauri::async_runtime::spawn_blocking(move || {
        zerus_client_core::run::run_cancellable(&hgs, &call, &[], 64 * 1024, &cancel)
            .map_err(|_| OPEN_FAILED.to_owned())
    })
    .await
    .map_err(|_| OPEN_FAILED.to_owned())?
}

#[tauri::command]
pub(crate) async fn native_ui_open(
    app: AppHandle,
    state: State<'_, crate::commands::AppState>,
    request: Request,
    reload: bool,
    embedded: bool,
) -> Result<Status, String> {
    let pump = state.pump();
    available(&pump, &request)?;
    let ui = app.state::<NativeUi>().inner().clone();
    if ui
        .busy
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(CONNECTING.into());
    }
    let _busy = Busy(ui.busy.clone());
    let cancel = Arc::new(AtomicBool::new(false));
    let generation = {
        let mut inner = lock(&ui.inner);
        if let Some(old) = inner.cancel.take() {
            old.store(true, Ordering::Relaxed);
        }
        inner.generation = inner.generation.saturating_add(1);
        inner.request = Some(request.clone());
        inner.cancel = Some(cancel.clone());
        inner.active = None;
        inner.visible = false;
        inner.revision = inner.revision.saturating_add(1);
        inner.generation
    };
    on_main(&app, renderer::hide_all).await?;
    ui.publish_for(&app, &request, generation, Phase::Connecting, CONNECTING);
    let hgs = zerus_client_core::default_hgs();
    let result = read(&hgs, &request, cancel, false).await.and_then(parse);
    if !ui.current(&request, generation) {
        return Err(CHANGED.into());
    }
    available(&pump, &request)?;
    match result {
        Ok(launch) if embedded => {
            renderer::open(&app, request, generation, launch, &hgs, reload).await?
        }
        Ok(_) => ui.publish_for(&app, &request, generation, Phase::Browser, BROWSER),
        Err(text) => ui.publish_for(&app, &request, generation, Phase::Failed, &text),
    }
    ui.status().ok_or_else(|| CHANGED.into())
}

#[tauri::command]
pub(crate) async fn native_ui_browser(
    app: AppHandle,
    state: State<'_, crate::commands::AppState>,
    request: Request,
) -> Result<(), String> {
    let pump = state.pump();
    available(&pump, &request)?;
    let ui = app.state::<NativeUi>().inner().clone();
    if ui
        .busy
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(CONNECTING.into());
    }
    let _busy = Busy(ui.busy.clone());
    let cancel = Arc::new(AtomicBool::new(false));
    lock(&ui.inner).cancel = Some(cancel.clone());
    let exit = read(&zerus_client_core::default_hgs(), &request, cancel, true).await?;
    match exit {
        Exit::Code { code: 0, .. } => Ok(()),
        _ => Err(OPEN_FAILED.into()),
    }
}

#[tauri::command]
pub(crate) async fn native_ui_retire(app: AppHandle) -> Result<(), String> {
    on_main(&app, |app| {
        let ui = app.state::<NativeUi>();
        {
            let mut inner = lock(&ui.inner);
            inner.generation = inner.generation.saturating_add(1);
            if let Some(cancel) = inner.cancel.take() {
                cancel.store(true, Ordering::Relaxed);
            }
            inner.request = None;
            inner.active = None;
            inner.visible = false;
            inner.bounds = None;
        }
        renderer::hide_all(app)
    })
    .await
}

#[tauri::command]
pub(crate) async fn native_ui_visible(
    app: AppHandle,
    nonce: String,
    visible: bool,
) -> Result<(), String> {
    on_main(&app, move |app| {
        let ui = app.state::<NativeUi>();
        {
            let mut inner = lock(&ui.inner);
            if inner
                .request
                .as_ref()
                .is_none_or(|request| request.nonce != nonce)
            {
                return Ok(());
            }
            inner.visible = visible;
        }
        renderer::place(app)
    })
    .await
}

#[tauri::command]
pub(crate) async fn native_ui_bounds(app: AppHandle, bounds: Bounds) -> Result<(), String> {
    if !bounds.valid() {
        return Err("Invalid native UI bounds".into());
    }
    on_main(&app, move |app| {
        let ui = app.state::<NativeUi>();
        {
            let mut inner = lock(&ui.inner);
            if inner
                .request
                .as_ref()
                .is_none_or(|request| request.nonce != bounds.nonce)
            {
                return Ok(());
            }
            inner.bounds = Some(bounds);
        }
        renderer::place(app)
    })
    .await
}

#[cfg(test)]
mod tests;
