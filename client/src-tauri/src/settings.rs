//! Settings: the client's local preferences (the core's `settings` store in
//! `<data_dir>/settings.json`), read and written for the webview. See
//! `docs/superpowers/plans/2026-10-28-tauri-client-phase3b-settings.md`.
//!
//! Rust code reads a setting from the managed [`Store`], for example
//! `app.state::<settings::Store>().0.number("processes/activeSeconds")`.

pub mod skins;

use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};
use zerus_client_core::settings::Settings;

/// The identity that replaces Qt at the switch: only it seeds from Qt's
/// store, once. The dev identity never does.
pub const SWITCH_IDENTITY: &str = crate::identity::SWITCH_ID;

/// How long quitting waits for the last change to reach the disk.
pub const EXIT_FLUSH: Duration = Duration::from_millis(750);

/// The app's one settings store.
pub struct Store(pub Settings);

pub fn file(replay: bool) -> &'static str {
    if replay {
        "settings-replay.json"
    } else {
        "settings.json"
    }
}

/// Settings > Appearance > Keep Zerus above other windows (WindowLayer.cpp).
pub const ALWAYS_ON_TOP: &str = "workspace/alwaysOnTop";

/// Opens the store in `dir`; every change is announced as
/// `settings-changed` with its key and new value, and Keep Zerus above other
/// windows applies to the main window at once.
pub fn open(app: &AppHandle, dir: &Path, replay: bool) -> Store {
    let settings = Settings::open(dir.join(file(replay)));
    // Natively drawn text (tray, menu bar) follows the language saved at startup.
    crate::native_text::configure(&settings.text("workspace/language"));
    let handle = app.clone();
    settings.subscribe(Box::new(move |key, value, revision| {
        let _ = handle.emit(
            "settings-changed",
            json!({ "key": key, "value": value, "revision": revision }),
        );
        // G1: turning the resident session host on starts it at once; a replay
        // never reaches a real CLI.
        if !replay {
            crate::resident_host::changed(key, value);
        }
    }));
    // G1: at startup, and every minute while the app lives (main.cpp:490-493).
    if !replay {
        crate::resident_host::spawn(settings.clone());
    }
    Store(settings)
}

/// `hgs --version` on this computer, for Settings > About
/// (SettingsPage.h:147-150); `None` when it cannot be read.
#[tauri::command]
pub async fn hgs_version() -> Option<String> {
    tauri::async_runtime::spawn_blocking(|| {
        let local = zerus_client_core::ComputerId::local();
        zerus_client_core::reads::read(
            &zerus_client_core::default_hgs(),
            &local,
            &zerus_client_core::reads::Read::Version,
        )
        .ok()
        .and_then(|version| version.as_str().map(str::to_owned))
    })
    .await
    .ok()
    .flatten()
}

/// The repository Settings > About opens (SettingsPage.h:151-154). The
/// address is fixed here: the webview cannot pass one.
pub const REPOSITORY: &str = "https://github.com/ufna/zerus";
pub const FORK_REPOSITORY: &str = "https://github.com/WhiteWh/zerus";

/// Opens the repository in the default browser.
#[tauri::command]
pub fn open_repository() -> Result<(), String> {
    // WhiteWh fork: a fork build opens the fork, as About names it.
    let fork = option_env!("ZERUS_FORK_BUILD").is_some_and(|build| build.trim().parse::<u64>().is_ok_and(|build| build > 0));
    let repository = if fork { FORK_REPOSITORY } else { REPOSITORY };
    open_url(repository).map_err(|error| format!("Could not open {repository}: {error}"))
}

/// Opens an address the client fixed (never one from the webview) in the
/// default browser.
pub fn open_url(url: &str) -> std::io::Result<()> {
    let program = if cfg!(target_os = "macos") {
        "/usr/bin/open"
    } else if cfg!(windows) {
        "explorer.exe"
    } else {
        "xdg-open"
    };
    let mut child = crate::child_environment::command(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // Reaped off the caller: the opener may stay until the browser starts.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Every setting, as Qt would read it, and the store's revision.
#[tauri::command]
pub async fn settings_get(store: State<'_, Store>) -> Result<Value, String> {
    let (values, revision) = store.0.all_tracked();
    Ok(json!({ "values": values, "revision": revision }))
}

/// The two hgs-owned settings Settings > Sessions shows (SettingsPage.h:242-250):
/// "Ask before an agent starts subagents" and "Block DeepSeek session-log
/// upload". The core builds the person's own `hgs <command> get|set --json`;
/// a replay without an explicit hgs never reaches a real CLI.
#[tauri::command]
pub async fn hgs_setting(
    call: zerus_client_core::hgs_settings::HgsSettingCall,
) -> Result<zerus_client_core::hgs_settings::HgsSettingOutcome, String> {
    if crate::actions::blocked() {
        return Err(crate::screens::REPLAY_NEEDS_HGS.into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        zerus_client_core::hgs_settings::run(&zerus_client_core::default_hgs(), &call)
    })
    .await
    .map_err(|error| error.to_string())
}

/// Stores one setting and answers with the value now in effect (clamped as
/// Qt clamps it; `null` for a removed override) and the store's revision after
/// the change.
#[tauri::command]
pub async fn settings_set(
    app: AppHandle,
    store: State<'_, Store>,
    key: String,
    value: Value,
) -> Result<Value, String> {
    if crate::desktop_service::mode()
        && (key == "automaticApprovals/enabled" || key.starts_with("automaticApprovals/category/"))
    {
        return Err(
            "Automatic approval settings must be changed through the background service.".into(),
        );
    }
    if crate::desktop_service::mode() && key == "notifications/optionalQuestions" {
        return Err(zerus_client_core::source!(
            "Notification settings must be changed through the background service."
        )
        .into());
    }
    let (value, revision) = if key == ALWAYS_ON_TOP {
        let on = value.as_bool().ok_or("Keep Above requires a boolean.")?;
        crate::window_layer::request(&app, on).await?
    } else {
        store.0.set_tracked(&key, value)?
    };
    Ok(json!({ "value": value, "revision": revision }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_settings_use_their_own_store() {
        assert_eq!(file(false), "settings.json");
        assert_eq!(file(true), "settings-replay.json");
    }
}
