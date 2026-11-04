//! Preserve the platform opener; a local Linux WebKit launch sanitizes its child.
use std::path::Path;
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

#[cfg(any(feature = "media", all(feature = "live", target_os = "linux")))]
pub fn url(app: &AppHandle, url: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if std::env::var_os(crate::child_environment::WEBKIT_MARKER).is_some() {
        return crate::settings::open_url(url).map_err(|error| error.to_string());
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|error| error.to_string())
}

pub fn path(app: &AppHandle, path: &Path) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if std::env::var_os(crate::child_environment::WEBKIT_MARKER).is_some() {
        path.metadata().map_err(|error| error.to_string())?;
        return crate::settings::open_url(&path.to_string_lossy())
            .map_err(|error| error.to_string());
    }
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(|error| error.to_string())
}
