//! Settings > Files (DownloadCacheSettings.cpp): the downloaded files
//! folder, how much its copies take, and the size limit that trims them.
//! The core's download_cache runs on one worker thread of its own, as Qt
//! runs it on a one-thread pool; the folder's lock also orders it with the
//! Qt client and with Download and open.

use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Manager, State};
use zerus_client_core::desktop::cache_maintenance::{Phase, Timing};
use zerus_client_core::download_cache::{self, Usage};

use crate::settings::Store;

type Job = Box<dyn FnOnce() + Send>;

/// The worker the folder's work runs on.
#[derive(Clone)]
pub struct Files(Arc<Mutex<Sender<Job>>>);

/// Starts the worker.
pub fn start() -> Files {
    let (jobs, queue) = mpsc::channel::<Job>();
    let started = std::thread::Builder::new()
        .name("download-cache".into())
        .spawn(move || {
            for job in queue {
                job();
            }
        });
    if let Err(error) = started {
        eprintln!("zerus-client: downloaded files: {error}");
    }
    Files(Arc::new(Mutex::new(jobs)))
}

impl Files {
    /// Runs `work` on the worker and waits for it.
    pub(crate) fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, String> {
        let (done, answer) = mpsc::channel();
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(Box::new(move || {
                let _ = done.send(work());
            }))
            .map_err(|_| "The downloaded files worker stopped.".to_owned())?;
        answer
            .recv()
            .map_err(|_| "The downloaded files worker stopped.".to_owned())
    }
}

/// The page: the folder, the limit in bytes (0: never clear) and its use.
#[derive(Clone, Debug, Serialize)]
pub struct FilesView {
    pub folder: String,
    pub limit: u64,
    pub bytes: u64,
    pub files: usize,
    pub error: Option<String>,
}

pub(crate) fn folder(app: &AppHandle) -> PathBuf {
    let store = app.state::<Store>();
    download_cache::folder(
        &store.0.text("downloads/folder"),
        app.path().download_dir().ok().as_deref(),
        &app.path().home_dir().unwrap_or_default(),
    )
}

pub(crate) fn limit(app: &AppHandle) -> u64 {
    app.state::<Store>()
        .0
        .get("downloads/cacheBytes")
        .as_u64()
        .unwrap_or(0)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as i64)
}

/// DownloadCacheSettings::refresh: with a limit, a trim to it; otherwise
/// the use as it is.
async fn refresh(app: &AppHandle, files: &Files, change: Option<u64>) -> Result<FilesView, String> {
    let root = folder(app);
    let service_mode = crate::desktop_service::mode();
    let mut service_error = None;
    if service_mode {
        let service = app
            .state::<crate::commands::AppState>()
            .desktop_service()
            .ok_or("Downloaded files maintenance is unavailable from the desktop service.")?;
        match service
            .cache_pass(root.clone(), change, Timing::Immediate)
            .await
        {
            Ok(Some(status)) => {
                if change.is_some() && !status.policy_saved {
                    return Err("The downloaded files limit was not saved.".into());
                }
                // This mirrors correlated service data for display. It is
                // never read back as service consent or execution permission.
                app.state::<Store>()
                    .0
                    .set("downloads/cacheBytes", json!(status.limit))?;
                if status.phase == Phase::Unknown {
                    service_error=Some("Some downloaded files have an unknown removal result and need review. They will not be retried.".into());
                } else if status.phase == Phase::Refused {
                    service_error=Some("The downloaded files could not be verified. Existing files were preserved.".into());
                }
            }
            Ok(None) => (),
            Err(error) => return Err(error),
        }
    } else if let Some(bytes) = change {
        app.state::<Store>()
            .0
            .set("downloads/cacheBytes", json!(bytes))?;
    }
    let limit = limit(app);
    let path = root.clone();
    let worker = files.clone();
    let usage: Usage = tauri::async_runtime::spawn_blocking(move || {
        worker.run(move || {
            if !service_mode && limit > 0 {
                download_cache::trim(&path, limit, now_ms())
            } else {
                download_cache::usage(&path)
            }
        })
    })
    .await
    .map_err(|error| error.to_string())??;
    Ok(FilesView {
        folder: root.to_string_lossy().into_owned(),
        limit,
        bytes: usage.bytes,
        files: usage.files,
        error: service_error.or(usage.error),
    })
}

/// The folder and its use now.
#[tauri::command(async)]
pub async fn files_usage(app: AppHandle, files: State<'_, Files>) -> Result<FilesView, String> {
    refresh(&app, &files, None).await
}

/// Apply: stores the limit (0: never clear), then trims to it.
#[tauri::command(async)]
pub async fn files_apply_limit(
    app: AppHandle,
    files: State<'_, Files>,
    bytes: u64,
) -> Result<FilesView, String> {
    refresh(&app, &files, Some(bytes)).await
}

/// Opens the folder in the file manager; the address is the client's own.
/// Off the main thread: the folder may be on a mount that hangs.
#[tauri::command(async)]
pub fn files_open_folder(app: AppHandle) -> Result<(), String> {
    let root = folder(&app);
    // A link is not followed.
    if !std::fs::symlink_metadata(&root).is_ok_and(|meta| meta.file_type().is_dir()) {
        return Err("The downloaded files folder is unavailable or is a link.".into());
    }
    crate::settings::open_url(&root.to_string_lossy()).map_err(|error| error.to_string())
}
