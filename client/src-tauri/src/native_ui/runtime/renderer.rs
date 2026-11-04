//! Native child placement and lifetime. No renderer may run before its
//! platform guards are installed; unsupported platforms use browser fallback.
use super::{lock, NativeUi, Phase, Request, BROWSER, FAILED};
#[cfg(target_os = "linux")]
use super::{LOADING, STOPPED};
use crate::native_ui::Launch;
use std::path::Path;
use tauri::{AppHandle, Manager, Webview};

pub(super) struct View {
    #[cfg(target_os = "linux")]
    pub label: String,
    pub key: String,
    pub webview: Webview,
    pub phase: Phase,
    #[cfg(target_os = "linux")]
    pub seen: u64,
}

pub(super) fn hide_all(app: &AppHandle) -> Result<(), String> {
    let views: Vec<_> = app
        .state::<NativeUi>()
        .inner
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .views
        .iter()
        .map(|view| view.webview.clone())
        .collect();
    let mut result = Ok(());
    for view in views {
        if view.hide().is_err() {
            result = Err(FAILED.to_owned());
        }
    }
    result
}

pub(super) fn place(app: &AppHandle) -> Result<(), String> {
    let ui = app.state::<NativeUi>();
    let current = {
        let inner = lock(&ui.inner);
        inner
            .request
            .clone()
            .map(|request| (request, inner.generation))
    };
    if let Some((request, generation)) = current {
        if let Err(reason) =
            super::available(&app.state::<crate::commands::AppState>().pump(), &request)
        {
            ui.publish_for(
                app,
                &request,
                generation,
                if reason == super::OFFLINE {
                    Phase::Offline
                } else {
                    Phase::Stopped
                },
                &reason,
            );
            return hide_all(app);
        }
    }
    let (views, active, bounds, visible) = {
        let inner = lock(&ui.inner);
        (
            inner
                .views
                .iter()
                .map(|view| (view.key.clone(), view.webview.clone(), view.phase))
                .collect::<Vec<_>>(),
            inner.active.clone(),
            inner.bounds.clone().filter(|bounds| {
                inner
                    .request
                    .as_ref()
                    .is_some_and(|request| request.nonce == bounds.nonce)
            }),
            inner.visible && !inner.dialog,
        )
    };
    for (key, view, phase) in views {
        if visible && active.as_ref() == Some(&key) && phase == Phase::Ready {
            if let Some(_bounds) = &bounds {
                #[cfg(target_os = "linux")]
                crate::native_ui::linux::place(
                    &view,
                    _bounds.x,
                    _bounds.y,
                    _bounds.width,
                    _bounds.height,
                )
                .map_err(|_| FAILED.to_owned())?;
                view.show().map_err(|_| FAILED.to_owned())?;
                continue;
            }
        }
        view.hide().map_err(|_| FAILED.to_owned())?;
    }
    Ok(())
}

pub(super) async fn open(
    app: &AppHandle,
    request: Request,
    generation: u64,
    launch: Launch,
    hgs: &Path,
    reload: bool,
) -> Result<(), String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (launch, hgs, reload);
        app.state::<NativeUi>()
            .publish_for(app, &request, generation, Phase::Browser, BROWSER);
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        let source = serde_json::to_string(&(hgs.to_string_lossy(), &request.target.computer))
            .map_err(|_| FAILED)?;
        let key = launch.key(&source);
        super::on_main(app, move |app| {
            open_linux(app, request, generation, launch, key, reload)
        })
        .await
    }
}

#[cfg(target_os = "linux")]
fn active(app: &AppHandle, label: &str) -> bool {
    let ui = app.state::<NativeUi>();
    let request = {
        let inner = lock(&ui.inner);
        if !inner
            .views
            .iter()
            .any(|view| view.label == label && inner.active.as_ref() == Some(&view.key))
        {
            return false;
        }
        inner.request.clone()
    };
    request.is_some_and(|request| {
        super::available(&app.state::<crate::commands::AppState>().pump(), &request).is_ok()
    })
}

#[cfg(target_os = "linux")]
fn load(app: &AppHandle, label: &str, event: crate::native_ui::linux::Load) {
    use crate::native_ui::linux::Load;
    let phase = match event {
        Load::Loading => Phase::Loading,
        Load::Ready => Phase::Ready,
        Load::Failed => Phase::Failed,
        Load::Stopped => Phase::Stopped,
    };
    let ui = app.state::<NativeUi>();
    {
        let mut inner = lock(&ui.inner);
        let Some(view) = inner.views.iter_mut().find(|view| view.label == label) else {
            return;
        };
        view.phase = phase;
    }
    if !active(app, label) {
        return;
    }
    let current = {
        let inner = lock(&ui.inner);
        inner
            .views
            .iter()
            .any(|view| view.label == label && inner.active.as_ref() == Some(&view.key))
            .then(|| {
                inner
                    .request
                    .clone()
                    .map(|request| (request, inner.generation))
            })
            .flatten()
    };
    if let Some((request, generation)) = current {
        ui.publish_for(
            app,
            &request,
            generation,
            phase,
            match phase {
                Phase::Loading => LOADING,
                Phase::Failed => FAILED,
                Phase::Stopped => STOPPED,
                _ => "",
            },
        );
    }
    let _ = place(app);
}

#[cfg(target_os = "linux")]
fn open_linux(
    app: &AppHandle,
    request: Request,
    generation: u64,
    launch: Launch,
    key: String,
    reload: bool,
) -> Result<(), String> {
    use std::sync::Arc;
    use tauri::webview::{NewWindowResponse, WebviewBuilder};
    let ui = app.state::<NativeUi>().inner().clone();
    if !ui.current(&request, generation) {
        return Err(super::CHANGED.into());
    }
    let (existing, evicted, label) = {
        let mut inner = lock(&ui.inner);
        inner.serial = inner.serial.saturating_add(1);
        let serial = inner.serial;
        let existing = inner
            .views
            .iter_mut()
            .find(|view| view.key == key)
            .map(|view| {
                view.seen = serial;
                (view.webview.clone(), view.phase)
            });
        let evicted = if existing.is_none() && inner.views.len() >= 4 {
            let index = inner
                .views
                .iter()
                .enumerate()
                .min_by_key(|(_, view)| view.seen)
                .map(|(index, _)| index)
                .unwrap_or(0);
            Some(inner.views.remove(index))
        } else {
            None
        };
        inner.active = Some(key.clone());
        (existing, evicted, format!("native-ui-{serial}"))
    };
    if let Some(view) = evicted {
        if view.webview.close().is_err() {
            lock(&ui.inner).views.push(view);
            return Err(FAILED.into());
        }
    }
    if let Some((view, phase)) = existing {
        if reload || matches!(phase, Phase::Failed | Phase::Stopped) {
            ui.publish_for(app, &request, generation, Phase::Loading, LOADING);
            view.navigate(launch.url().clone())
                .map_err(|_| FAILED.to_owned())?;
        } else {
            ui.publish_for(
                app,
                &request,
                generation,
                phase,
                if phase == Phase::Loading { LOADING } else { "" },
            );
        }
        return place(app);
    }
    let load_app = app.clone();
    let load_label = label.clone();
    let browser_app = app.clone();
    let browser_label = label.clone();
    let hooks = crate::native_ui::linux::Hooks {
        load: Arc::new(move |event| load(&load_app, &load_label, event)),
        browser: Arc::new(move |url| {
            if !active(&browser_app, &browser_label) {
                return;
            }
            let ui = browser_app.state::<NativeUi>().inner().clone();
            let original = {
                let inner = lock(&ui.inner);
                if !inner.visible || inner.dialog {
                    return;
                }
                inner
                    .request
                    .clone()
                    .map(|request| (request, inner.generation))
            };
            let Some((request, generation)) = original else {
                return;
            };
            if ui
                .link_busy
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                )
                .is_err()
            {
                return;
            }
            let app = browser_app.clone();
            let label = browser_label.clone();
            tauri::async_runtime::spawn_blocking(move || {
                let _guard = super::Busy(ui.link_busy.clone());
                if !ui.current(&request, generation) || !active(&app, &label) {
                    return;
                }
                let _ = crate::external_open::url(&app, url.as_str());
            });
        }),
    };
    let save_app = app.clone();
    let save_label = label.clone();
    let builder = WebviewBuilder::new(
        &label,
        tauri::WebviewUrl::External("about:blank".parse().map_err(|_| FAILED)?),
    )
    .incognito(true)
    .devtools(false)
    .on_new_window(|_, _| NewWindowResponse::Deny)
    .on_web_resource_request(|request, response| {
        let scheme = request.uri().scheme_str().unwrap_or_default();
        let host = request.uri().host().unwrap_or_default();
        if matches!(scheme, "tauri" | "asset" | "ipc" | "zerus-file" | "file")
            || matches!(
                host,
                "tauri.localhost" | "asset.localhost" | "ipc.localhost" | "zerus-file.localhost"
            )
        {
            *response.status_mut() = tauri::http::StatusCode::FORBIDDEN;
            *response.body_mut() = std::borrow::Cow::Borrowed(&[]);
            response.headers_mut().clear();
        }
    })
    .on_download(move |_, event| match event {
        tauri::webview::DownloadEvent::Requested { destination, .. } => {
            save(&save_app, &save_label, destination)
        }
        _ => false,
    });
    let main = app.get_window(crate::MAIN_WINDOW).ok_or(FAILED)?;
    let view = match main.add_child(
        builder,
        tauri::LogicalPosition::new(0.0, 0.0),
        tauri::LogicalSize::new(1.0, 1.0),
    ) {
        Ok(view) => view,
        Err(_) => {
            ui.publish_for(app, &request, generation, Phase::Browser, BROWSER);
            return Ok(());
        }
    };
    if view.hide().is_err() {
        let _ = view.close();
        return Err(FAILED.into());
    }
    if crate::main_window(app)
        .as_ref()
        .is_none_or(|main| crate::native_ui::linux::mount(main, &view).is_err())
    {
        let _ = view.close();
        ui.publish_for(app, &request, generation, Phase::Browser, BROWSER);
        return Ok(());
    }
    // On Tauri's event-loop thread this callback runs synchronously. Only
    // about:blank exists until all native guards have been installed.
    let installed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let result = installed.clone();
    let policy = launch.clone();
    let hooked = view.with_webview(move |platform| {
        if crate::native_ui::linux::install(&platform.inner(), policy, hooks).is_ok() {
            result.store(true, std::sync::atomic::Ordering::Release);
        }
    });
    if hooked.is_err() || !installed.load(std::sync::atomic::Ordering::Acquire) {
        let _ = view.close();
        ui.publish_for(app, &request, generation, Phase::Browser, BROWSER);
        return Ok(());
    }
    {
        let mut inner = lock(&ui.inner);
        let seen = inner.serial;
        inner.views.push(View {
            label,
            key,
            webview: view.clone(),
            phase: Phase::Loading,
            seen,
        });
    }
    ui.publish_for(app, &request, generation, Phase::Loading, LOADING);
    view.navigate(launch.url().clone())
        .map_err(|_| FAILED.to_owned())?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn save(app: &AppHandle, label: &str, destination: &mut std::path::PathBuf) -> bool {
    use gtk::prelude::*;
    if !active(app, label) {
        return false;
    }
    let ui = app.state::<NativeUi>();
    let request = {
        let inner = lock(&ui.inner);
        if inner.dialog || !inner.visible || !matches!(inner.phase, Phase::Ready | Phase::Loading) {
            return false;
        }
        inner.request.clone()
    };
    let Some(window) = crate::main_window(app).and_then(|window| window.gtk_window().ok()) else {
        return false;
    };
    lock(&ui.inner).dialog = true;
    if hide_all(app).is_err() {
        lock(&ui.inner).dialog = false;
        return false;
    }
    let dialog = gtk::FileChooserNative::new(
        Some("Save file"),
        Some(&window),
        gtk::FileChooserAction::Save,
        Some("Save"),
        Some("Cancel"),
    );
    dialog.set_modal(true);
    dialog.set_do_overwrite_confirmation(true);
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| name.len() <= 255 && !name.chars().any(char::is_control))
        .unwrap_or("download");
    dialog.set_current_name(name);
    let selected = if dialog.run() == gtk::ResponseType::Accept {
        dialog.filename()
    } else {
        None
    };
    dialog.hide();
    dialog.destroy();
    let current = lock(&ui.inner).request == request;
    lock(&ui.inner).dialog = false;
    let _ = place(app);
    if !current || !active(app, label) {
        return false;
    }
    if let Some(path) = selected {
        *destination = path;
        true
    } else {
        false
    }
}
