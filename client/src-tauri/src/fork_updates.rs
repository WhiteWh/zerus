//! WhiteWh fork, task #63 (owner, 2026-11-06): the fork's update channel in
//! the Tauri desktop, the same as the Qt GUI's ForkUpdater. The rules, the
//! staging and the helper live in `zerus_client_core::fork_update`; this
//! module fetches over HTTPS, keeps the timers (two minutes after start, then
//! hourly, only in a build that embeds the feed), unpacks with Windows
//! `tar.exe` and, on Restart, starts the helper and quits as Quit does, so
//! drafts are saved. Settings > Updates shows it through `UpdatesView::fork`.
//!
//! `ZERUS_FORK_BUILD` and `ZERUS_FORK_FEED_URL` come from the build
//! environment (`build.rs`), as the Qt build embeds them.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;
use tauri::{AppHandle, Manager};
use zerus_client_core::fork_update::{self, Config, Download, ForkUpdater, ForkView};

static FORK: OnceLock<Arc<Mutex<ForkUpdater>>> = OnceLock::new();

fn lock(updater: &Mutex<ForkUpdater>) -> MutexGuard<'_, ForkUpdater> {
    updater.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What this build was made with.
pub fn config() -> Config {
    Config::from_build(option_env!("ZERUS_FORK_BUILD"), option_env!("ZERUS_FORK_FEED_URL"))
}

/// The channel's state for this installation; only a portable Windows build
/// can check or install.
fn updater() -> &'static Arc<Mutex<ForkUpdater>> {
    FORK.get_or_init(|| {
        let executable = std::env::current_exe().unwrap_or_default();
        let layout = fork_update::layout_for(&executable);
        let portable = cfg!(windows) && fork_update::portable(&layout);
        let commit = if portable { fork_update::source_commit(&layout.install) } else { String::new() };
        Arc::new(Mutex::new(ForkUpdater::new(config(), layout, portable, commit)))
    })
}

/// Settings > Updates: the fork channel, or None outside a fork build's
/// portable installation (an upstream build never shows it).
pub fn view() -> Option<ForkView> {
    let updater = lock(updater());
    (updater.supported() || updater.fork_build()).then(|| updater.view())
}

/// A fork build is never replaced by upstream's channels.
pub fn fork_build() -> bool {
    lock(updater()).fork_build()
}

/// A verified fork build waits for Restart.
pub fn ready() -> bool {
    lock(updater()).ready()
}

fn changed(app: &AppHandle) {
    if let Some(updates) = app.try_state::<crate::updates::Updates>() {
        updates.refresh();
    }
}

/// Starts the automatic checks of a build that embeds the feed.
pub fn start(app: &AppHandle) {
    if !lock(updater()).automatic() {
        return;
    }
    let app = app.clone();
    let _ = std::thread::Builder::new().name("fork-updates".into()).spawn(move || {
        std::thread::sleep(fork_update::FIRST_CHECK);
        loop {
            run_check(&app, true);
            std::thread::sleep(fork_update::CHECK_INTERVAL);
        }
    });
}

fn agent(timeout: Option<Duration>) -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig};
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_global(timeout)
        // GitHub answers release assets with a redirect to its storage; only
        // HTTPS hops are followed, as Qt's NoLessSafeRedirectPolicy does.
        .https_only(true)
        .max_redirects(5)
        .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
        .build()
        .new_agent()
}

fn fetch_feed(url: &str) -> Result<Vec<u8>, String> {
    let mut response = agent(Some(Duration::from_secs(30))).get(url).call().map_err(|error| error.to_string())?;
    if response.status().as_u16() != 200 {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    response
        .body_mut()
        .with_config()
        .limit(fork_update::FEED_LIMIT as u64 + 1)
        .read_to_vec()
        .map_err(|error| error.to_string())
}

fn fetch_zip(download: &Download) -> Result<(u64, [u8; 32]), String> {
    let mut response = agent(None).get(&download.feed.url).call().map_err(|error| error.to_string())?;
    if response.status().as_u16() != 200 {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    let mut file = std::fs::File::create(download.zip()).map_err(|error| error.to_string())?;
    let reader = response.body_mut().with_config().limit(download.feed.size + 1).reader();
    fork_update::receive(reader, &mut file, download.feed.size)
}

fn system_root() -> PathBuf {
    std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
}

/// Windows 10 and later ship bsdtar, which reads zip archives and refuses
/// absolute and parent paths.
fn unpack(zip: &Path, folder: &Path) -> Result<(), String> {
    let output = crate::child_environment::command(system_root().join("System32").join("tar.exe"))
        .arg("-xf")
        .arg(zip)
        .arg("-C")
        .arg(folder)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Err(if text.trim().is_empty() { format!("tar exited with {}", output.status) } else { text })
}

/// Reads the feed and prepares a newer build; the lock is never held while
/// the network or tar works.
fn run_check(app: &AppHandle, automatic: bool) {
    let Some(url) = lock(updater()).begin_check(automatic) else {
        return;
    };
    changed(app);
    let feed = fetch_feed(&url);
    let download = lock(updater()).feed_read(feed);
    changed(app);
    if let Some(download) = download {
        prepare(app, &download);
    }
}

fn prepare(app: &AppHandle, download: &Download) {
    let received = fetch_zip(download);
    // Unpack only what matches the feed; the updater checks it again.
    let matches = matches!(&received, Ok((size, hash)) if *size == download.feed.size && *hash == download.feed.sha256);
    let unpacked = if matches { unpack(&download.zip(), &download.folder) } else { Ok(()) };
    lock(updater()).downloaded(download, received, move |_, _| unpacked);
    changed(app);
}

#[tauri::command]
pub fn fork_updates_check(app: AppHandle) {
    let _ = std::thread::Builder::new().name("fork-updates-check".into()).spawn(move || run_check(&app, false));
}

/// The owner confirmed replacing this unnumbered build with the offered one.
#[tauri::command]
pub fn fork_updates_confirm(app: AppHandle) {
    let _ = std::thread::Builder::new().name("fork-updates-confirm".into()).spawn(move || {
        let download = lock(updater()).confirm_replacement();
        changed(&app);
        if let Some(download) = download {
            prepare(&app, &download);
        }
    });
}

/// Restart to finish the fork update: the helper starts detached without a
/// console window, then Zerus quits as Quit does (drafts saved).
pub fn restart(app: &AppHandle) -> Result<(), String> {
    // The bundled mobile connector runs from the installation; without this
    // the helper waits for it and the update never lands.
    crate::mobile::stop_connector_for_update();
    let temp = std::env::temp_dir();
    let mut updater = lock(updater());
    let script = temp.join(format!("zerus-fork-install-{}.ps1", updater.ready_build()));
    let command = updater
        .start_install(&script, std::process::id(), &system_root(), &temp)
        .map_err(|text| text.english())?;
    let mut process = crate::child_environment::command(&command.program);
    process
        .args(&command.args)
        .current_dir(&command.working_directory)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: the helper outlives Zerus without a console; its
        // banner is its only window.
        process.creation_flags(0x0800_0000);
    }
    if let Err(error) = process.spawn() {
        updater.install_failed(error.to_string());
        drop(updater);
        changed(app);
        return Err(error.to_string());
    }
    drop(updater);
    changed(app);
    crate::exit_flush::quit_with_flush(app, 0);
    Ok(())
}
