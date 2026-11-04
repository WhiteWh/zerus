//! The menu-bar app's lifecycle, as the Qt GUI behaves on macOS
//! (`tray/src/main.cpp`, `tray/src/MacAppVisibility.mm`): closing the window
//! hides it and the app stays in the menu bar; a Dock icon shows only while
//! the window is open; a Dock click opens or restores it when hidden; quitting
//! removes the single-instance socket. Builds without a menu-bar item close
//! as before. `lib.rs` calls [`on_event`] from its run loop (Decision 15).
use tauri::{AppHandle, Manager, RunEvent, WebviewWindow, WindowEvent};

use crate::MAIN_WINDOW;

/// The close button only hides the window of an app with a menu-bar item
/// (`QApplication::setQuitOnLastWindowClosed(false)`, `main.cpp:279`).
pub fn close_hides(menu_bar: bool) -> bool {
    menu_bar
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Regular,
    Accessory,
}

/// A normal app while its window is open, shown or minimised, else a
/// menu-bar app (`MacAppVisibility.mm:65-81`).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn policy(window_open: bool) -> Policy {
    if window_open {
        Policy::Regular
    } else {
        Policy::Accessory
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReopenAction {
    Restore,
    Show,
    Nothing,
}

/// A Dock click leaves a visible window alone, restores a minimised one
/// as Qt does, and opens a hidden or missing window (owner rule, .66.2.37).
/// An existing window is represented by (minimized, visible).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn reopen_action(window: Option<(bool, bool)>) -> ReopenAction {
    match window {
        Some((_, true)) => ReopenAction::Nothing,
        Some((true, false)) => ReopenAction::Restore,
        _ => ReopenAction::Show,
    }
}

/// The window opens at start for `--sessions`, and always without a menu-bar item.
pub fn window_at_start(opens_sessions: bool, menu_bar: bool) -> bool {
    opens_sessions || !menu_bar
}

/// Starts the fleet at launch, before any window loads (menu-bar builds). A
/// source that fails to start, such as a missing `ZERUS_CLIENT_REPLAY`
/// fixture, is logged and the app runs on: the webview's own `start` tries
/// again and reports it when the window opens. Returns whether it started.
#[cfg_attr(not(feature = "live"), allow(dead_code))]
pub fn start_fleet(start: impl FnOnce() -> Result<String, String>) -> bool {
    match start() {
        Ok(_) => true,
        Err(error) => {
            eprintln!("zerus-client: the data source did not start at launch: {error}");
            false
        }
    }
}

/// Kept in the app state.
pub struct Lifecycle {
    pub menu_bar: bool,
}

fn menu_bar(app: &AppHandle) -> bool {
    app.try_state::<Lifecycle>()
        .is_some_and(|lifecycle| lifecycle.menu_bar)
}

/// The run loop's events this module handles.
pub fn on_event(app: &AppHandle, event: &RunEvent) {
    match event {
        RunEvent::ExitRequested { code, api, .. } => {
            match app
                .state::<crate::exit_flush::ExitFlush>()
                .decision(menu_bar(app), *code)
            {
                crate::exit_flush::Decision::Allow => {}
                crate::exit_flush::Decision::Prevent => api.prevent_exit(),
                crate::exit_flush::Decision::Flush(code) => {
                    api.prevent_exit();
                    crate::exit_flush::quit_with_flush(app, code);
                }
            }
        }
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::CloseRequested { api, .. },
            ..
        } if label == MAIN_WINDOW => {
            api.prevent_close();
            if close_hides(menu_bar(app)) {
                hide_main(app);
            } else {
                crate::exit_flush::quit_with_flush(app, 0);
            }
        }
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => reopen(app),
        RunEvent::Exit => exit(app),
        _ => {}
    }
}

/// Opens Zerus: Qt's `TrayAgent::showSessions` (`TrayAgent.cpp:781-783`).
pub fn show_main(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let window = match crate::main_window(app) {
        Some(window) => window,
        None => crate::create_main_window(app)?,
    };
    #[cfg(feature = "live")]
    if !std::env::var("ZERUS_CLIENT_PERF").is_ok_and(|mode| mode == "auto")
        && crate::window_layer::show(app, window.clone())
    {
        return Ok(window);
    }
    if window.is_minimized()? {
        window.unminimize()?;
    }
    window.show()?;
    window.set_focus()?;
    sync_policy(app);
    #[cfg(all(windows, feature = "live"))]
    crate::platform::windows::tray::apply_badge(app);
    Ok(window)
}

/// Windows cannot construct WebView2 synchronously in a native event callback.
/// The existing window-layer worker owns the first creation as well as its
/// level and presentation. Other platforms keep their main-thread behavior.
pub fn request_main(app: &AppHandle) -> Result<(), String> {
    #[cfg(all(windows, feature = "live"))]
    {
        crate::window_layer::open(app)
    }
    #[cfg(not(all(windows, feature = "live")))]
    show_main(app)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub fn hide_main(app: &AppHandle) {
    #[cfg(feature = "live")]
    crate::window_layer::hidden(app);
    if let Some(window) = crate::main_window(app) {
        let _ = window.hide();
    }
    sync_policy(app);
}

#[cfg(target_os = "macos")]
fn window_open(app: &AppHandle) -> bool {
    crate::main_window(app).is_some_and(|window| {
        window.is_visible().unwrap_or(false) || window.is_minimized().unwrap_or(false)
    })
}

#[cfg(target_os = "macos")]
fn native(policy: Policy) -> tauri::ActivationPolicy {
    match policy {
        Policy::Regular => tauri::ActivationPolicy::Regular,
        Policy::Accessory => tauri::ActivationPolicy::Accessory,
    }
}

/// Applies [`policy`] to the app (macOS menu-bar builds only).
pub fn sync_policy(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    if menu_bar(app) {
        let _ = app.set_activation_policy(native(policy(window_open(app))));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// The policy the app launches with, set before the run loop starts (macOS
/// menu-bar builds). tao makes every app a normal one when it finishes
/// launching, before `setup` runs, so a hidden start would show a Dock icon
/// first. Qt's bundle starts as a menu-bar app (`LSUIElement`,
/// `tray/packaging/Info.plist:21`).
#[cfg(target_os = "macos")]
pub fn prepare(app: &mut tauri::App, menu_bar: bool, opens_sessions: bool) {
    if menu_bar {
        app.set_activation_policy(native(policy(window_at_start(opens_sessions, menu_bar))));
    }
}

#[cfg(target_os = "macos")]
fn reopen(app: &AppHandle) {
    let window = crate::main_window(app);
    let state = window.as_ref().map(|window| {
        (
            window.is_minimized().unwrap_or(false),
            window.is_visible().unwrap_or(false),
        )
    });
    match reopen_action(state) {
        ReopenAction::Restore => {
            if let Some(window) = window {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }
        ReopenAction::Show => {
            let _ = show_main(app);
        }
        ReopenAction::Nothing => {}
    }
}

/// The one run-loop exit hook (Decision 15). `RunEvent::Exit` ends every
/// quit: Quit, a signal (`signals.rs`) and the updater's quit event (tao's
/// `applicationWillTerminate`). Other work before the process ends (drafts,
/// organization) joins here, before the socket goes. The main-thread
/// watchdog stops first, so a slow flush never ends in an abort (Task 8);
/// then the Dock badge goes, as `~MacApplicationBadge` clears it. Qt's `QLocalServer`
/// outlives the tray agent whose `aboutToQuit` flushes (`main.cpp:340`,
/// `:378`), so the socket is removed last, as `QLocalServer::close` does.
fn exit(app: &AppHandle) {
    #[cfg(feature = "live")]
    if let Some(native) = app.try_state::<crate::native_ui::runtime::NativeUi>() {
        native.shutdown();
    }
    // Quitting is not a hang (`Watchdog::stop`).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(dog) = app.try_state::<crate::watchdog::Watchdog>() {
        dog.stop();
    }
    #[cfg(all(target_os = "linux", feature = "live"))]
    {
        crate::platform::linux_notify::stop(app);
        crate::platform::linux_tray::clear_badge(app);
    }
    #[cfg(all(target_os = "macos", feature = "live"))]
    crate::platform::macos_menubar::clear_badge(app);
    #[cfg(all(windows, feature = "live"))]
    crate::platform::windows::tray::stop(app);
    // Settings, the organization, voice, composers and drafts reach the disk
    // while the socket still stands, as Qt's tray agent flushes before its
    // server goes.
    #[cfg(feature = "live")]
    crate::exit_work(app);
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    if let Some(state) = app.try_state::<crate::instance::InstanceState>() {
        state.release();
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    let _ = app;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dock_icon_shows_while_the_window_is_open() {
        assert_eq!(policy(true), Policy::Regular);
        assert_eq!(policy(false), Policy::Accessory);
    }

    #[test]
    fn a_dock_click_opens_hidden_or_missing_windows_and_restores_minimised_ones() {
        assert_eq!(reopen_action(None), ReopenAction::Show);
        assert_eq!(reopen_action(Some((false, false))), ReopenAction::Show);
        assert_eq!(reopen_action(Some((true, false))), ReopenAction::Restore);
        assert_eq!(reopen_action(Some((false, true))), ReopenAction::Nothing);
        assert_eq!(reopen_action(Some((true, true))), ReopenAction::Nothing);
    }

    #[test]
    fn the_window_opens_at_start_only_when_asked_in_the_menu_bar() {
        assert!(
            !window_at_start(false, true),
            "the LaunchAgent start stays in the menu bar"
        );
        assert!(window_at_start(true, true));
        assert!(
            window_at_start(false, false),
            "without a menu-bar item the window is the only way in"
        );
        assert!(close_hides(true));
        assert!(!close_hides(false));
    }

    #[test]
    fn a_source_that_fails_at_launch_leaves_the_app_running() {
        assert!(!start_fleet(|| Err(
            "replay: sample.ndjson: not found".to_owned()
        )));
        assert!(start_fleet(|| Ok("live".to_owned())));
    }
}
