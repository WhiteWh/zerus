//! File-reference dialogs. Paths chosen by the cache/save dialog stay in Rust;
//! downloads outlive their dialog, while cache work uses Settings > Files' worker.
use crate::composer::fetch::Opener;
use crate::composer::Spawn;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};
use zerus_client_core::download_cache::{self, Source};
use zerus_client_core::external_file::{self, Origin, Outcome};
use zerus_client_core::paths::{self, Style};
use zerus_client_core::{file_download, session_file_reference, terminal_launch, ComputerId};
mod downloads;
use downloads::{Answer, Downloads};
mod ssh;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionFileView {
    pub id: u64,
    pub revision: u64,
    pub computer: String,
    pub key: String,
    pub href: String,
    pub path: String,
    pub location: String,
    pub remote: bool,
    pub title: String,
    pub local_available: bool,
    pub can_download: bool,
    pub checking: bool,
    pub downloading: bool,
    pub picking: bool,
    pub hint: String,
    pub error: String,
    pub closed: bool,
}
#[derive(Clone)]
struct Dialog {
    computer: String,
    key: String,
    href: String,
    resolved: String,
    shown: String,
    location: String,
    local: Option<PathBuf>,
    revision: u64,
    check: u64,
    checking: bool,
    downloading: bool,
    picking: bool,
    hint: String,
    error: String,
    ssh: Option<ssh::Identity>,
    opening_ssh: bool,
}
pub struct SessionFiles {
    app: AppHandle,
    opener: Opener,
    spawn: Spawn,
    hgs: PathBuf,
    dir: PathBuf,
    terminal: Mutex<terminal_launch::Launcher>,
    downloads: Arc<Downloads>,
    cache: crate::files::Files,
    next: AtomicU64,
    dialogs: Mutex<HashMap<u64, Dialog>>,
    trim: mpsc::SyncSender<()>,
    trim_busy: AtomicBool,
}
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_millis() as i64)
}
fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}
fn saved(path: &Path) -> String {
    format!("Saved on this computer: {}\nOpen the downloaded copy, or download again to get an updated file.", path.display())
}
fn readable(path: &Path) -> bool {
    path.is_file() && std::fs::File::open(path).is_ok()
}
impl SessionFiles {
    pub fn new(
        app: AppHandle,
        dir: PathBuf,
        hgs: PathBuf,
        opener: Opener,
        spawn: Spawn,
    ) -> Arc<Self> {
        let cache = app.state::<crate::files::Files>().inner().clone();
        let allowed = crate::actions::actions_allowed(
            std::env::var("ZERUS_CLIENT_REPLAY").ok().as_deref(),
            std::env::var("ZERUS_CLIENT_HGS").ok().as_deref(),
        );
        let download_hgs = hgs.clone();
        let downloads = Downloads::new(
            cache.clone(),
            spawn.clone(),
            Arc::new(move |host, path, output| {
                if !allowed {
                    return Err(crate::actions::REPLAY_NEEDS_HGS.into());
                }
                file_download::download(&download_hgs, &[], host, path, output)
            }),
        );
        let (trim, timer) = mpsc::sync_channel(1);
        let me = Arc::new(Self {
            app,
            opener,
            spawn,
            hgs,
            dir,
            terminal: Mutex::new(terminal_launch::Launcher::new(Default::default())),
            downloads,
            cache,
            next: AtomicU64::new(1),
            dialogs: Mutex::new(HashMap::new()),
            trim,
            trim_busy: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&me);
        // One resettable grace timer, rather than a sleeping task per download.
        let _ = std::thread::Builder::new()
            .name("download-trim-timer".into())
            .spawn(move || {
                while timer.recv().is_ok() {
                    loop {
                        match timer.recv_timeout(Duration::from_millis(
                            download_cache::OPENING_GRACE_MS as u64 + 5000,
                        )) {
                            Ok(()) => continue,
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                if let Some(me) = weak.upgrade() {
                                    me.run_trim(false);
                                }
                                break;
                            }
                            Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        }
                    }
                }
            });
        me
    }
    fn online(&self, computer: &str) -> bool {
        self.app
            .state::<crate::commands::AppState>()
            .pump()
            .online(computer)
    }
    fn view(&self, id: u64, closed: bool) -> Option<SessionFileView> {
        let dialogs = lock(&self.dialogs);
        let d = dialogs.get(&id)?;
        let remote = !d.computer.is_empty();
        Some(SessionFileView {
            id,
            revision: d.revision,
            computer: d.computer.clone(),
            key: d.key.clone(),
            href: d.href.clone(),
            path: d.shown.clone(),
            location: d.location.clone(),
            remote,
            title: if remote {
                format!("File on {}", d.computer)
            } else {
                "File on this machine".into()
            },
            local_available: d.local.is_some(),
            can_download: remote && !d.resolved.is_empty() && self.online(&d.computer),
            checking: d.checking,
            downloading: d.downloading,
            picking: d.picking,
            hint: d.hint.clone(),
            error: d.error.clone(),
            closed,
        })
    }
    fn emit(&self, id: u64) {
        if let Some(view) = self.view(id, false) {
            let _ = self.app.emit("session-file-changed", view);
        }
    }
    fn notice(&self, text: String, warn: bool) {
        let _ = self.app.emit(
            "session-file-notice",
            serde_json::json!({"text":text,"warn":warn}),
        );
    }
    fn dialog(
        &self,
        computer: String,
        key: String,
        href: String,
        cwd: String,
    ) -> Result<SessionFileView, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let remote = !computer.is_empty();
        let style = if remote { Style::Posix } else { paths::build() };
        let target = session_file_reference::parse(&href, style);
        let home = self.app.path().home_dir().ok();
        let mut resolved = session_file_reference::resolve(
            &target,
            &cwd,
            if remote {
                None
            } else {
                home.as_deref().and_then(Path::to_str)
            },
            style,
        );
        if remote && resolved.is_empty() && target.path.starts_with("~/") {
            resolved = target.path.clone();
        }
        let hint = if remote {
            if !self.online(&computer) {
                format!("{computer} is offline. Connect to this machine to download its files.")
            } else if resolved.is_empty() {
                "The session folder is unknown. Copy the path or use the SSH terminal to locate the file.".into()
            } else {
                "Download… lets you choose where to save. Download and open saves a copy in Downloads/Zerus Files and opens it with your default application.".into()
            }
        } else if resolved.is_empty() {
            "The session folder is unknown. Copy the path to locate this file.".into()
        } else {
            "Open with your default application, or copy the path.".into()
        };
        let pump = self.app.state::<crate::commands::AppState>().pump();
        let ssh = ssh::Identity::capture(
            &computer,
            pump.row(&computer, &key).as_ref(),
            pump.machine(&ComputerId(computer.clone())).as_ref(),
        );
        let mut dialogs = lock(&self.dialogs);
        // Closed dialogs never retain transfer state here; callbacks own their
        // finite completion independently. Bound abandoned open-dialog handles.
        if dialogs.len() >= 64 {
            return Err("The file reference is unavailable.".into());
        }
        dialogs.insert(
            id,
            Dialog {
                computer,
                key,
                href,
                local: (!remote && !resolved.is_empty()).then(|| PathBuf::from(&resolved)),
                shown: if resolved.is_empty() {
                    target.path.clone()
                } else {
                    resolved.clone()
                },
                resolved,
                location: target.location,
                revision: 0,
                check: 0,
                checking: false,
                downloading: false,
                picking: false,
                hint,
                error: if target.path.is_empty() {
                    "The file reference is invalid.".into()
                } else {
                    String::new()
                },
                ssh,
                opening_ssh: false,
            },
        );
        drop(dialogs);
        self.view(id, false)
            .ok_or_else(|| "The file reference is unavailable.".into())
    }
    fn check(self: &Arc<Self>, id: u64) -> Option<SessionFileView> {
        let (source, generation) = {
            let mut dialogs = lock(&self.dialogs);
            let d = dialogs.get_mut(&id)?;
            if d.computer.is_empty() || d.resolved.is_empty() || d.downloading || d.picking {
                drop(dialogs);
                return self.view(id, false);
            }
            d.check += 1;
            d.revision += 1;
            d.checking = true;
            (
                Source {
                    host: d.computer.clone(),
                    path: d.resolved.clone(),
                },
                d.check,
            )
        };
        let root = crate::files::folder(&self.app);
        let me = self.clone();
        (self.spawn)(Box::new(move || {
            let result = me
                .cache
                .run(move || download_cache::lookup(&root, &source, now_ms()))
                .and_then(|r| r);
            {
                let mut dialogs = lock(&me.dialogs);
                let Some(d) = dialogs.get_mut(&id) else {
                    return;
                };
                if d.check != generation || d.downloading {
                    return;
                }
                d.checking = false;
                d.revision += 1;
                match result {
                    Ok(Some(path)) => {
                        d.hint = saved(&path);
                        d.local = Some(path);
                    }
                    Ok(None) => {}
                    Err(error) => d.error = format!("Cannot check the downloaded copy: {error}"),
                }
            }
            me.emit(id);
        }));
        self.view(id, false)
    }
    fn start_download(
        self: &Arc<Self>,
        id: u64,
        destination: Destination,
        open: bool,
    ) -> Option<SessionFileView> {
        let root = crate::files::folder(&self.app);
        let cached = matches!(destination, Destination::Cache);
        let (source, force) = {
            let mut dialogs = lock(&self.dialogs);
            let d = dialogs.get_mut(&id)?;
            if d.downloading
                || d.picking
                || d.computer.is_empty()
                || d.resolved.is_empty()
                || !self.online(&d.computer)
            {
                drop(dialogs);
                return self.view(id, false);
            }
            let shown = match &destination {
                Destination::Cache => &root,
                Destination::Direct(path) => path,
            };
            d.hint = format!(
                "Downloading from {} to {}…\nThe download continues if you close this dialog.",
                d.computer,
                shown.display()
            );
            d.check += 1;
            d.revision += 1;
            d.checking = false;
            d.downloading = true;
            d.error.clear();
            (
                Source {
                    host: d.computer.clone(),
                    path: d.resolved.clone(),
                },
                d.local.is_some(),
            )
        };
        self.emit(id);
        let me = self.clone();
        let done = Box::new(move |answer| me.finish_download(id, answer, open, cached));
        match destination {
            Destination::Cache => self.downloads.cached(root, source, force, done),
            Destination::Direct(path) => self.downloads.direct(source, path, done),
        }
        self.view(id, false)
    }
    fn finish_download(self: &Arc<Self>, id: u64, result: Answer, open: bool, cached: bool) {
        if let Ok((path, warning)) = &result {
            external_file::mark_downloaded(path);
            if let Some(warning) = warning {
                self.notice(warning.clone(), true);
            }
            if cached {
                self.run_trim(true);
                let _ = self.trim.try_send(());
            }
        }
        let unavailable = if result.is_err() {
            let copy = lock(&self.dialogs).get(&id).and_then(|d| d.local.clone());
            copy.is_some_and(|path| !readable(&path))
        } else {
            false
        };
        let shown = {
            let mut dialogs = lock(&self.dialogs);
            if open && result.is_ok() {
                // Commit closing before another command can start a new request
                // on the completed dialog. Its callback still owns the open.
                dialogs.remove(&id).is_some()
            } else if let Some(d) = dialogs.get_mut(&id) {
                d.downloading = false;
                d.revision += 1;
                match &result {
                    Ok((path, _)) => {
                        d.local = Some(path.clone());
                        d.hint = saved(path);
                    }
                    Err(error) => {
                        if unavailable {
                            d.local = None;
                        }
                        d.hint.clear();
                        d.error = format!("Download failed: {error}");
                    }
                }
                true
            } else {
                false
            }
        };
        match result {
            Ok((path, _)) if open => {
                if shown {
                    self.close(id);
                }
                self.open(path, Origin::OtherMachine, false);
            }
            Ok((path, _)) if !shown => {
                self.notice(format!("Downloaded to {}", path.display()), false)
            }
            Err(error) if !shown => self.notice(format!("Download failed: {error}"), true),
            _ => self.emit(id),
        }
    }
    fn run_trim(self: &Arc<Self>, after_download: bool) {
        if crate::desktop_service::mode() {
            if self.trim_busy.swap(true, Ordering::AcqRel) {
                return;
            }
            let service = self
                .app
                .state::<crate::commands::AppState>()
                .desktop_service();
            let root = crate::files::folder(&self.app);
            let me = self.clone();
            tauri::async_runtime::spawn(async move {
                let timing = if after_download {
                    zerus_client_core::desktop::cache_maintenance::Timing::AfterOpeningGrace
                } else {
                    zerus_client_core::desktop::cache_maintenance::Timing::Immediate
                };
                let result = match service {
                    Some(service) => service.cache_pass(root, None, timing).await.map(|_| ()),
                    None => Err(
                        "Downloaded files maintenance is unavailable from the desktop service."
                            .into(),
                    ),
                };
                me.trim_busy.store(false, Ordering::Release);
                if let Err(error) = result {
                    me.notice(error, true);
                }
            });
            return;
        }
        let limit = crate::files::limit(&self.app);
        if limit == 0 || self.trim_busy.swap(true, Ordering::AcqRel) {
            return;
        }
        let root = crate::files::folder(&self.app);
        let me = self.clone();
        (self.spawn)(Box::new(move || {
            let result = me
                .cache
                .run(move || download_cache::trim(&root, limit, now_ms()));
            me.trim_busy.store(false, Ordering::Release);
            match result {
                Ok(usage) => {
                    if let Some(error) = usage.error {
                        me.notice(error, true);
                    }
                }
                Err(error) => me.notice(error, true),
            }
        }));
    }
    fn open(self: &Arc<Self>, path: PathBuf, origin: Origin, reveal: bool) {
        let me = self.clone();
        // Use the persistent window after the dialog has been closed. Never
        // dispatch from a transient save dialog's native window.
        let app = self.app.clone();
        let scheduled = app.run_on_main_thread(move || {
            let spawn = me.spawn.clone();
            spawn(Box::new(move || {
                let outcome = if reveal {
                    if (me.opener.reveal)(&path) {
                        Outcome::Opened
                    } else {
                        Outcome::RevealFailed
                    }
                } else {
                    external_file::open(
                        &path,
                        origin,
                        |p| (me.opener.open)(p),
                        |p| (me.opener.reveal)(p),
                    )
                };
                match outcome {
                    Outcome::Opened => {}
                    Outcome::Revealed => me.notice(external_file::revealed_notice(&path), false),
                    Outcome::OpenFailed => {
                        me.notice("No application could open this file.".into(), true)
                    }
                    Outcome::RevealFailed => {
                        me.notice(external_file::reveal_failed_notice(&path), true)
                    }
                }
            }));
        });
        if scheduled.is_err() {
            self.notice("No application could open this file.".into(), true);
        }
    }
    fn act(self: &Arc<Self>, id: u64, action: &str) -> Option<SessionFileView> {
        if action == "open_ssh" {
            return self.open_ssh(id);
        }
        let d = lock(&self.dialogs).get(&id)?.clone();
        if d.downloading || d.picking {
            return self.view(id, false);
        }
        let path = d.local?;
        let remote = !d.computer.is_empty();
        let (path, reveal, error) = match action {
            "open_folder" if !remote => {
                let parent = path.parent()?.to_path_buf();
                let error = (!parent.is_dir())
                    .then_some("This folder is no longer available at this path.");
                (parent, false, error)
            }
            "open_file" | "open_copy" | "show_file" => {
                let available = if remote {
                    readable(&path)
                } else {
                    path.exists()
                };
                let error = (!available).then_some(if remote {
                    "The local copy is no longer available."
                } else {
                    "This file is no longer available at this path."
                });
                (path, action == "show_file", error)
            }
            _ => return self.view(id, false),
        };
        if let Some(error) = error {
            {
                let mut dialogs = lock(&self.dialogs);
                let d = dialogs.get_mut(&id)?;
                d.error = error.into();
                d.revision += 1;
            }
            self.emit(id);
            return self.view(id, false);
        }
        let mut closed = self.view(id, true)?;
        closed.revision += 1;
        self.close(id);
        self.open(
            path,
            if remote {
                Origin::OtherMachine
            } else {
                Origin::ThisComputer
            },
            reveal,
        );
        Some(closed)
    }
    // Called only by the explicit button command, off the GUI thread. Offline
    // peers remain usable for host-key/login repair, as in Qt: no remote probe.
    fn open_ssh(&self, id: u64) -> Option<SessionFileView> {
        let (computer, key, identity) = {
            let mut dialogs = lock(&self.dialogs);
            let d = dialogs.get_mut(&id)?;
            if d.computer.is_empty() || d.opening_ssh {
                drop(dialogs);
                return self.view(id, false);
            }
            d.opening_ssh = true;
            (d.computer.clone(), d.key.clone(), d.ssh.clone())
        };
        let result = (|| {
            if crate::actions::blocked() {
                return Err(crate::actions::REPLAY_NEEDS_HGS.to_owned());
            }
            // Resolve only the saved alias, never a webview-supplied host/path.
            let machines = zerus_client_core::reads::read(
                &self.hgs,
                &ComputerId::local(),
                &zerus_client_core::reads::Read::Machines,
            )
            .map_err(|error| error.to_string())?;
            let mut terminal = lock(&self.terminal);
            terminal.set_settings(crate::desk::terminal_settings(&self.dir));
            let pump = self.app.state::<crate::commands::AppState>().pump();
            let row = pump.row(&computer, &key);
            let machine = pump.machine(&ComputerId(computer.clone()));
            if !ssh::current(identity.as_ref(), row.as_ref(), machine.as_ref())
                || !ssh::configured(&computer, &machines)
            {
                return Err("The session state changed. Open its actions again.".into());
            }
            // Closing during the bounded local read cancels this unissued click.
            if !lock(&self.dialogs).contains_key(&id) {
                return Err("The file reference is unavailable.".into());
            }
            ssh::open(&mut terminal, &self.hgs, &computer)
        })();
        match result {
            Ok(advisory) => {
                if let Some(text) = advisory {
                    crate::desk::window_notice(&self.app, terminal_launch::ADVISORY_TITLE, &text);
                }
                let mut view = self.view(id, true)?;
                view.revision += 1;
                self.close(id);
                Some(view)
            }
            Err(error) => {
                let text = format!("Could not open the SSH terminal: {error}");
                if let Some(d) = lock(&self.dialogs).get_mut(&id) {
                    d.opening_ssh = false;
                    d.error = text.clone();
                    d.revision += 1;
                } else {
                    self.notice(text, true);
                }
                self.emit(id);
                self.view(id, false)
            }
        }
    }
    fn close(&self, id: u64) {
        lock(&self.dialogs).remove(&id);
        let _ = self.app.emit(
            "session-file-changed",
            SessionFileView {
                id,
                closed: true,
                ..Default::default()
            },
        );
    }
}
#[derive(Clone)]
enum Destination {
    Cache,
    Direct(PathBuf),
}
#[tauri::command]
pub fn session_file_dialog(
    files: tauri::State<'_, Arc<SessionFiles>>,
    computer: String,
    key: String,
    href: String,
    cwd: String,
) -> Result<SessionFileView, String> {
    files.dialog(computer, key, href, cwd)
}
#[tauri::command]
pub fn session_file_check(
    files: tauri::State<'_, Arc<SessionFiles>>,
    id: u64,
) -> Option<SessionFileView> {
    files.check(id)
}
#[tauri::command]
pub fn session_file_download(
    files: tauri::State<'_, Arc<SessionFiles>>,
    id: u64,
    open: bool,
) -> Option<SessionFileView> {
    files.start_download(id, Destination::Cache, open)
}
#[tauri::command]
pub async fn session_file_save(
    app: AppHandle,
    files: tauri::State<'_, Arc<SessionFiles>>,
    id: u64,
) -> Result<Option<SessionFileView>, String> {
    let start = app
        .path()
        .download_dir()
        .or_else(|_| app.path().home_dir())
        .map_err(|error| error.to_string())?;
    let name = {
        let mut dialogs = lock(&files.dialogs);
        let Some(d) = dialogs.get_mut(&id) else {
            return Ok(None);
        };
        if d.downloading
            || d.picking
            || d.computer.is_empty()
            || d.resolved.is_empty()
            || !files.online(&d.computer)
        {
            return Ok(None);
        }
        d.picking = true;
        d.check += 1;
        d.checking = false;
        d.revision += 1;
        download_cache::filename(&d.resolved)
    };
    files.emit(id);
    let picked = tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        app.dialog()
            .file()
            .set_title("Save local copy")
            .set_directory(start)
            .set_file_name(name)
            .blocking_save_file()
    })
    .await;
    {
        let mut dialogs = lock(&files.dialogs);
        if let Some(d) = dialogs.get_mut(&id) {
            d.picking = false;
            d.revision += 1;
        }
    }
    files.emit(id);
    let picked = picked.map_err(|error| error.to_string())?;
    let Some(path) = picked.and_then(|file| file.into_path().ok()) else {
        return Ok(files.view(id, false));
    };
    Ok(files.start_download(id, Destination::Direct(path), false))
}
#[tauri::command]
pub fn session_file_external(url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://"))
        || url.chars().any(char::is_control)
    {
        return Err("The external link is invalid.".into());
    }
    crate::settings::open_url(&url).map_err(|error| format!("Could not open {url}: {error}"))
}

#[tauri::command(async)]
pub fn session_file_act(
    files: tauri::State<'_, Arc<SessionFiles>>,
    id: u64,
    action: String,
) -> Result<Option<SessionFileView>, String> {
    Ok(files.act(id, &action))
}

#[tauri::command]
pub fn session_file_close(files: tauri::State<'_, Arc<SessionFiles>>, id: u64) {
    files.close(id);
}
