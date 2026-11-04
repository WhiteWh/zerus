//! Which WebKit renderer this process uses (Linux; a no-op elsewhere).
//!
//! On NVIDIA Wayland the shared-memory renderer repaints the whole window every
//! frame and costs about five times the CPU of DMA-BUF with explicit sync off
//! (`bench/results/spike-29-webkitgtk-idle-2026-10-28.md`, "Client-side renderer
//! policy"). WebKit reads its renderer variables when GTK starts, so `choose`
//! runs after GUI admission, before GTK or any listener/renderer thread.
//!
//! `ZERUS_CLIENT_RENDERER=auto|dmabuf|shm` is the only switch (default `auto`).
//! The decision itself is the pure `decide`; detection reads a root directory so
//! tests can use a fake tree.

use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::{Mutex, OnceLock};

#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "used only by the Linux choose()")
)]
const DMABUF_VAR: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "used only by the Linux choose()")
)]
const NV_SYNC_VAR: &str = "__NV_DISABLE_EXPLICIT_SYNC";
#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "used only by the Linux choose()")
)]
const MODE_VAR: &str = "ZERUS_CLIENT_RENDERER";
/// The client's identifier (tauri.conf.json); its data directory is named by it.
#[cfg(target_os = "linux")]
const IDENTIFIER: &str = "dev.zerus.next";
#[cfg(target_os = "linux")]
pub const MARKER_FILE: &str = "renderer-marker.json";

/// The environment the decision reads.
#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "used only by the Linux choose()")
)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    /// `ZERUS_CLIENT_RENDERER`.
    pub mode: Option<String>,
    /// `WEBKIT_DISABLE_DMABUF_RENDERER`.
    pub dmabuf_var: Option<String>,
    /// `__NV_DISABLE_EXPLICIT_SYNC`.
    pub nv_sync: Option<String>,
}

/// What the machine looks like.
#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "used only by the Linux choose()")
)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    pub nvidia: bool,
    /// The proprietary driver's version, the marker key.
    pub driver: Option<String>,
    pub wayland: bool,
}

/// The crash marker: a DMA-BUF start that has not reached a frame yet.
#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "used only by the Linux choose()")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub renderer: String,
    pub driver: String,
    pub written_at: u64,
}

#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "used only by the Linux choose()")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MarkerAction {
    /// No marker is involved in this start.
    Leave,
    /// Write a fresh marker (it is cleared by the first frame).
    Write,
    /// Delete the stale marker of another driver, then write a fresh one.
    Replace,
    /// Keep the marker found: the last DMA-BUF start never reached a frame.
    Keep,
}

#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "used only by the Linux choose()")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decision {
    /// `default` (WebKit's own choice), `dmabuf` or `shm`.
    pub renderer: &'static str,
    pub reason: String,
    pub nvidia: bool,
    pub wayland: bool,
    pub marker_state: &'static str,
    #[serde(skip)]
    pub marker: MarkerAction,
    /// Variables to set; never contains `__NV_DISABLE_EXPLICIT_SYNC` when the user set it.
    #[serde(skip)]
    pub set: Vec<(&'static str, &'static str)>,
    /// Lines to log once.
    #[serde(skip)]
    pub log: Vec<String>,
}

/// The pure decision. `marker` is what the marker file holds now.
#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "used only by the Linux choose()")
)]
pub fn decide(env: &Env, facts: &Facts, marker: Option<&Marker>) -> Decision {
    let mut log = Vec::new();
    let mode = match env.mode.as_deref().map(str::trim) {
        None | Some("") | Some("auto") => "auto",
        Some("dmabuf") => "dmabuf",
        Some("shm") => "shm",
        Some(other) => {
            log.push(format!(
                "renderer: unknown {MODE_VAR}={other:?}, using auto"
            ));
            "auto"
        }
    };
    let nv_wayland = facts.nvidia && facts.wayland;
    let mut set: Vec<(&'static str, &'static str)> = Vec::new();
    let nv_sync = |set: &mut Vec<(&'static str, &'static str)>| {
        if nv_wayland && env.nv_sync.is_none() {
            set.push((NV_SYNC_VAR, "1"));
        }
    };
    let finish = |renderer, reason: &str, marker_state, marker, set, log| Decision {
        renderer,
        reason: reason.to_owned(),
        nvidia: facts.nvidia,
        wayland: facts.wayland,
        marker_state,
        marker,
        set,
        log,
    };

    let dmabuf_path = match (mode, env.dmabuf_var.as_deref()) {
        ("shm", _) => {
            set.push((DMABUF_VAR, "1"));
            return finish(
                "shm",
                "ZERUS_CLIENT_RENDERER=shm",
                "not-used",
                MarkerAction::Leave,
                set,
                log,
            );
        }
        ("dmabuf", _) => Some("ZERUS_CLIENT_RENDERER=dmabuf"),
        ("auto", None) => {
            nv_sync(&mut set);
            let reason = if nv_wayland {
                "auto: NVIDIA Wayland, explicit sync off"
            } else {
                "auto: WebKit's own choice"
            };
            return finish("default", reason, "not-used", MarkerAction::Leave, set, log);
        }
        ("auto", Some("0")) => Some("auto: WEBKIT_DISABLE_DMABUF_RENDERER=0 chooses DMA-BUF"),
        _ => None,
    };
    let Some(reason) = dmabuf_path else {
        log.push("renderer: shared memory, set by the system".to_owned());
        return finish(
            "shm",
            "set by the system (WEBKIT_DISABLE_DMABUF_RENDERER)",
            "not-used",
            MarkerAction::Leave,
            set,
            log,
        );
    };

    let key = facts.driver.as_deref().unwrap_or("none");
    match marker {
        Some(found) if found.driver == key => {
            set.push((DMABUF_VAR, "1"));
            log.push(
                "renderer: DMA-BUF did not reach a frame last start; using shared memory"
                    .to_owned(),
            );
            finish(
                "shm",
                "the last DMA-BUF start did not reach a frame",
                "kept",
                MarkerAction::Keep,
                set,
                log,
            )
        }
        other => {
            set.push((DMABUF_VAR, "0"));
            nv_sync(&mut set);
            let (state, action) = if other.is_some() {
                ("retry-after-driver-change", MarkerAction::Replace)
            } else {
                ("written", MarkerAction::Write)
            };
            finish("dmabuf", reason, state, action, set, log)
        }
    }
}

/// Reads the machine facts below `root` (`/` in the app). `gdk_backend`,
/// `wayland_display` and `runtime_dir` are the environment values GDK looks at.
#[cfg(target_os = "linux")]
pub fn detect(
    root: &Path,
    gdk_backend: Option<&str>,
    wayland_display: Option<&str>,
    runtime_dir: Option<&Path>,
) -> Facts {
    let version = std::fs::read_to_string(root.join("proc/driver/nvidia/version")).ok();
    let drm_on_nvidia = std::fs::read_dir(root.join("sys/class/drm")).is_ok_and(|cards| {
        cards.flatten().any(|card| {
            let is_card = card.file_name().to_str().is_some_and(|n| {
                n.starts_with("card") && n[4..].chars().all(|c| c.is_ascii_digit())
            });
            is_card
                && std::fs::read_link(card.path().join("device/driver"))
                    .ok()
                    .and_then(|target| {
                        target
                            .file_name()
                            .map(|n| n.to_string_lossy().ends_with("nvidia"))
                    })
                    .unwrap_or(false)
        })
    });
    let modeset = std::fs::read_to_string(root.join("sys/module/nvidia_drm/parameters/modeset"))
        .is_ok_and(|v| v.trim() == "Y");
    let nvidia = version.is_some() && drm_on_nvidia && modeset;
    Facts {
        nvidia,
        driver: version.as_deref().and_then(driver_version),
        wayland: wayland_session(gdk_backend, wayland_display, runtime_dir),
    }
}

/// The version number of `/proc/driver/nvidia/version`'s first line, or the line itself.
#[cfg(target_os = "linux")]
fn driver_version(text: &str) -> Option<String> {
    let line = text.lines().next()?.trim();
    let token = line.split_whitespace().find(|t| {
        t.contains('.')
            && t.chars().all(|c| c.is_ascii_digit() || c == '.')
            && t.starts_with(|c: char| c.is_ascii_digit())
    });
    Some(token.unwrap_or(line))
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

/// Wayland as GDK and libwayland decide: the backend list does not start with
/// another backend (`*` and `wayland*` are Wayland-first), `XDG_RUNTIME_DIR` is
/// set, and the compositor socket exists: `WAYLAND_DISPLAY` (absolute, or below
/// the runtime directory) or, when that is unset, `wayland-0`. A named socket that
/// is missing is not Wayland; there is no fallback to `wayland-0`.
#[cfg(target_os = "linux")]
fn wayland_session(
    gdk_backend: Option<&str>,
    wayland_display: Option<&str>,
    runtime_dir: Option<&Path>,
) -> bool {
    use std::os::unix::fs::FileTypeExt;
    let first = gdk_backend.and_then(|b| b.split(',').next()).map(str::trim);
    if first.is_some_and(|b| !b.is_empty() && b != "*" && !b.starts_with("wayland")) {
        return false;
    }
    let Some(runtime) = runtime_dir.filter(|d| d.is_absolute()) else {
        return false;
    };
    let name = Path::new(
        wayland_display
            .filter(|n| !n.is_empty())
            .unwrap_or("wayland-0"),
    );
    let socket = if name.is_absolute() {
        name.to_path_buf()
    } else {
        runtime.join(name)
    };
    std::fs::metadata(socket).is_ok_and(|m| m.file_type().is_socket())
}

/// The client's private data directory, the one Tauri's `app_data_dir` gives on Linux.
#[cfg(target_os = "linux")]
fn data_dir() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let base =
        absolute("XDG_DATA_HOME").or_else(|| absolute("HOME").map(|h| h.join(".local/share")))?;
    Some(base.join(IDENTIFIER))
}

#[cfg(target_os = "linux")]
fn read_marker(path: &Path) -> Option<Marker> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Temporary file (0600) plus rename, so a crash never leaves a half marker.
#[cfg(target_os = "linux")]
fn write_marker(path: &Path, marker: &Marker) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(marker).map_err(std::io::Error::other)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(target_os = "linux")]
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The outcome for diagnostics and the marker path the first frame clears.
#[cfg(target_os = "linux")]
struct Chosen {
    decision: Decision,
    /// Set while this start's marker exists and the first frame should delete it.
    marker_path: Mutex<Option<PathBuf>>,
}

#[cfg(target_os = "linux")]
static CHOSEN: OnceLock<Chosen> = OnceLock::new();

/// Decides and applies the renderer. Call first in `main`, before any thread.
#[cfg(target_os = "linux")]
pub fn choose() {
    let var = |name: &str| std::env::var(name).ok();
    let env = Env {
        mode: var(MODE_VAR),
        dmabuf_var: var(DMABUF_VAR),
        nv_sync: var(NV_SYNC_VAR),
    };
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let facts = detect(
        Path::new("/"),
        var("GDK_BACKEND").as_deref(),
        var("WAYLAND_DISPLAY").as_deref(),
        runtime.as_deref(),
    );
    let path = data_dir().map(|d| d.join(MARKER_FILE));
    let marker = path.as_deref().and_then(read_marker);
    let mut decision = decide(&env, &facts, marker.as_ref());
    let mut owned_marker = None;
    if matches!(decision.marker, MarkerAction::Write | MarkerAction::Replace) {
        let fresh = Marker {
            renderer: "dmabuf".into(),
            driver: facts.driver.clone().unwrap_or_else(|| "none".into()),
            written_at: now(),
        };
        match path.as_deref().map(|p| write_marker(p, &fresh).map(|()| p)) {
            Some(Ok(p)) => owned_marker = Some(p.to_path_buf()),
            failure => {
                // Without the guard a crash would repeat on every start: stay on shared memory.
                let why = match failure {
                    Some(Err(error)) => error.to_string(),
                    _ => "no data directory".into(),
                };
                decision.log.push(format!(
                    "renderer: cannot write the crash marker ({why}); using shared memory"
                ));
                decision.set.retain(|(name, _)| *name != DMABUF_VAR);
                decision.set.push((DMABUF_VAR, "1"));
                decision.renderer = "shm";
                decision.reason = "the crash marker could not be written".into();
                decision.marker_state = "unwritable";
            }
        }
    }
    // SAFETY: GUI admission calls this before GTK or any thread exists, so no
    // other thread can read or write the environment concurrently.
    unsafe {
        for (name, value) in &decision.set {
            std::env::set_var(name, value);
        }
    }
    for line in &decision.log {
        eprintln!("zerus-client: {line}");
    }
    eprintln!(
        "zerus-client: renderer {} ({}); nvidia {}, wayland {}, marker {}",
        decision.renderer,
        decision.reason,
        decision.nvidia,
        decision.wayland,
        decision.marker_state
    );
    let _ = CHOSEN.set(Chosen {
        decision,
        marker_path: Mutex::new(owned_marker),
    });
}

#[cfg(not(target_os = "linux"))]
pub fn choose() {}

/// The decision as diagnostics: renderer, reason, nvidia, wayland, marker_state.
#[cfg(target_os = "linux")]
pub fn diagnostics() -> serde_json::Value {
    match CHOSEN.get() {
        Some(chosen) => serde_json::to_value(&chosen.decision).unwrap_or(serde_json::Value::Null),
        None => serde_json::Value::Null,
    }
}

#[cfg(not(target_os = "linux"))]
pub fn diagnostics() -> serde_json::Value {
    serde_json::Value::Null
}

/// Deletes the marker held in `slot` once; later calls find it empty and do nothing.
#[cfg(target_os = "linux")]
fn clear_marker(slot: &Mutex<Option<PathBuf>>) -> bool {
    let path = slot.lock().ok().and_then(|mut p| p.take());
    path.is_some_and(|p| std::fs::remove_file(p).is_ok())
}

/// The first frame after load reached the page: the DMA-BUF start worked.
#[tauri::command]
#[cfg(target_os = "linux")]
pub fn renderer_frame() -> bool {
    CHOSEN
        .get()
        .is_some_and(|chosen| clear_marker(&chosen.marker_path))
}

#[tauri::command]
#[cfg(not(target_os = "linux"))]
pub fn renderer_frame() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(mode: Option<&str>, var: Option<&str>, nv: Option<&str>) -> Env {
        Env {
            mode: mode.map(Into::into),
            dmabuf_var: var.map(Into::into),
            nv_sync: nv.map(Into::into),
        }
    }
    fn facts(nvidia: bool, wayland: bool) -> Facts {
        Facts {
            nvidia,
            wayland,
            driver: nvidia.then(|| "615.78.08".to_owned()),
        }
    }
    fn marker(driver: &str) -> Marker {
        Marker {
            renderer: "dmabuf".into(),
            driver: driver.into(),
            written_at: 1,
        }
    }
    const NV: (&str, &str) = (NV_SYNC_VAR, "1");

    #[test]
    fn auto_absent_on_nvidia_wayland_turns_explicit_sync_off() {
        let d = decide(&env(None, None, None), &facts(true, true), None);
        assert_eq!(
            (d.renderer, d.set, d.marker),
            ("default", vec![NV], MarkerAction::Leave)
        );
    }

    #[test]
    fn auto_absent_elsewhere_changes_nothing() {
        for f in [facts(false, true), facts(true, false), facts(false, false)] {
            let d = decide(&env(Some("auto"), None, None), &f, None);
            assert_eq!(
                (d.renderer, d.set.is_empty(), d.marker),
                ("default", true, MarkerAction::Leave)
            );
        }
    }

    #[test]
    fn auto_with_one_respects_the_system_and_logs_it() {
        for value in ["1", "true", ""] {
            let d = decide(&env(None, Some(value), None), &facts(true, true), None);
            assert_eq!(
                (d.renderer, d.set.is_empty(), d.marker),
                ("shm", true, MarkerAction::Leave)
            );
            assert_eq!(d.log, ["renderer: shared memory, set by the system"]);
        }
    }

    #[test]
    fn auto_with_zero_behaves_as_dmabuf_with_a_marker() {
        let d = decide(&env(None, Some("0"), None), &facts(true, true), None);
        assert_eq!(
            (d.renderer, d.marker, d.marker_state),
            ("dmabuf", MarkerAction::Write, "written")
        );
        assert_eq!(d.set, vec![(DMABUF_VAR, "0"), NV]);
        let again = decide(
            &env(None, Some("0"), None),
            &facts(true, true),
            Some(&marker("615.78.08")),
        );
        assert_eq!((again.renderer, again.marker), ("shm", MarkerAction::Keep));
    }

    #[test]
    fn dmabuf_without_a_marker_writes_one() {
        let d = decide(
            &env(Some("dmabuf"), Some("1"), None),
            &facts(true, true),
            None,
        );
        assert_eq!((d.renderer, d.marker), ("dmabuf", MarkerAction::Write));
        assert_eq!(d.set, vec![(DMABUF_VAR, "0"), NV]);
        let off_nvidia = decide(&env(Some("dmabuf"), None, None), &facts(false, true), None);
        assert_eq!(off_nvidia.set, vec![(DMABUF_VAR, "0")]);
    }

    #[test]
    fn dmabuf_with_a_marker_for_the_same_driver_falls_back_and_keeps_it() {
        let d = decide(
            &env(Some("dmabuf"), None, None),
            &facts(true, true),
            Some(&marker("615.78.08")),
        );
        assert_eq!(
            (d.renderer, d.marker, d.set),
            ("shm", MarkerAction::Keep, vec![(DMABUF_VAR, "1")])
        );
        assert_eq!(
            d.log,
            ["renderer: DMA-BUF did not reach a frame last start; using shared memory"]
        );
    }

    #[test]
    fn dmabuf_with_a_marker_for_another_driver_retries_once() {
        let d = decide(
            &env(Some("dmabuf"), None, None),
            &facts(true, true),
            Some(&marker("610.00")),
        );
        assert_eq!((d.renderer, d.marker), ("dmabuf", MarkerAction::Replace));
        assert_eq!(d.set[0], (DMABUF_VAR, "0"));
    }

    #[test]
    fn shm_forces_shared_memory_without_a_marker() {
        let d = decide(
            &env(Some("shm"), Some("0"), None),
            &facts(true, true),
            Some(&marker("615.78.08")),
        );
        assert_eq!(
            (d.renderer, d.set, d.marker),
            ("shm", vec![(DMABUF_VAR, "1")], MarkerAction::Leave)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn x11_changes_nothing_in_auto() {
        let temp = Temp::new("x11");
        let _compositor = std::os::unix::net::UnixListener::bind(temp.0.join("wayland-0")).unwrap();
        // The same socket is Wayland without GDK_BACKEND=x11 and not with it.
        let wayland = detect(Path::new("/nonexistent-root"), None, None, Some(&temp.0)).wayland;
        let x11 = detect(
            Path::new("/nonexistent-root"),
            Some("x11"),
            None,
            Some(&temp.0),
        )
        .wayland;
        assert!((wayland, x11) == (true, false));
        let nvidia = |wayland| Facts {
            nvidia: true,
            driver: Some("1.0".into()),
            wayland,
        };
        assert!(decide(&env(None, None, None), &nvidia(x11), None)
            .set
            .is_empty());
        assert!(!decide(&env(None, None, None), &nvidia(wayland), None)
            .set
            .is_empty());
    }

    #[test]
    fn a_user_set_explicit_sync_is_kept() {
        for mode in [None, Some("dmabuf")] {
            let d = decide(&env(mode, None, Some("0")), &facts(true, true), None);
            assert!(
                d.set.iter().all(|(name, _)| *name != NV_SYNC_VAR),
                "{:?}",
                d.set
            );
        }
    }

    #[test]
    fn an_unknown_value_counts_as_auto_and_is_logged_once() {
        let d = decide(&env(Some("gpu"), None, None), &facts(true, true), None);
        assert_eq!((d.renderer, d.set), ("default", vec![NV]));
        assert_eq!(d.log.len(), 1);
        assert!(d.log[0].contains("unknown"));
    }

    #[cfg(target_os = "linux")]
    struct Temp(PathBuf);
    #[cfg(target_os = "linux")]
    impl Temp {
        fn new(test: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("zerus-renderer-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Temp(path)
        }
    }
    #[cfg(target_os = "linux")]
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A fake /proc and /sys of an NVIDIA machine, plus a runtime dir.
    #[cfg(target_os = "linux")]
    fn fake_machine(root: &Path, modeset: &str) {
        std::fs::create_dir_all(root.join("proc/driver/nvidia")).unwrap();
        std::fs::write(root.join("proc/driver/nvidia/version"), "NVRM version: NVIDIA UNIX Open Kernel Module for x86_64  615.78.08  Release Build\nGCC version: gcc\n").unwrap();
        std::fs::create_dir_all(root.join("sys/class/drm/card0/device")).unwrap();
        std::fs::create_dir_all(root.join("sys/class/drm/card1/device")).unwrap();
        std::os::unix::fs::symlink(
            "../../../../bus/pci/drivers/i915",
            root.join("sys/class/drm/card0/device/driver"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            "../../../../bus/pci/drivers/nvidia",
            root.join("sys/class/drm/card1/device/driver"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("sys/module/nvidia_drm/parameters")).unwrap();
        std::fs::write(
            root.join("sys/module/nvidia_drm/parameters/modeset"),
            format!("{modeset}\n"),
        )
        .unwrap();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn detection_reads_a_fake_tree() {
        let temp = Temp::new("detect");
        let root = temp.0.join("root");
        fake_machine(&root, "Y");
        let runtime = temp.0.join("run");
        std::fs::create_dir_all(&runtime).unwrap();
        let _compositor =
            std::os::unix::net::UnixListener::bind(runtime.join("wayland-0")).unwrap();
        let f = detect(&root, None, None, Some(&runtime));
        assert_eq!(
            f,
            Facts {
                nvidia: true,
                driver: Some("615.78.08".into()),
                wayland: true
            }
        );
        // GDK_BACKEND lists that start with wayland or *, a relative and an absolute WAYLAND_DISPLAY.
        for backend in ["wayland,x11", "*", "*,x11"] {
            assert!(
                detect(&root, Some(backend), None, Some(&runtime)).wayland,
                "{backend}"
            );
        }
        let _second = std::os::unix::net::UnixListener::bind(runtime.join("wayland-1")).unwrap();
        assert!(detect(&root, None, Some("wayland-1"), Some(&runtime)).wayland);
        let _other = std::os::unix::net::UnixListener::bind(temp.0.join("other.sock")).unwrap();
        let absolute = temp.0.join("other.sock");
        assert!(detect(&root, None, absolute.to_str(), Some(&runtime)).wayland);
        // A named socket that is missing does not fall back to wayland-0 (which exists here).
        assert!(!detect(&root, None, Some("wayland-2"), Some(&runtime)).wayland);
        // No runtime directory: not Wayland, even with an absolute socket.
        assert!(!detect(&root, None, absolute.to_str(), None).wayland);
        // No socket, a plain file, or a forced X11 backend: not Wayland.
        assert!(!detect(&root, None, None, Some(&temp.0)).wayland);
        std::fs::write(temp.0.join("plain"), "").unwrap();
        assert!(!detect(&root, None, Some("plain"), Some(&temp.0)).wayland);
        assert!(!detect(&root, Some("x11"), None, Some(&runtime)).wayland);
        assert!(!detect(&root, Some("x11,wayland"), None, Some(&runtime)).wayland);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn detection_needs_all_three_nvidia_facts() {
        let temp = Temp::new("nvidia");
        fake_machine(&temp.0, "N");
        assert!(!detect(&temp.0, None, None, None).nvidia, "modeset N");
        std::fs::write(
            temp.0.join("sys/module/nvidia_drm/parameters/modeset"),
            "Y\n",
        )
        .unwrap();
        std::fs::remove_file(temp.0.join("sys/class/drm/card1/device/driver")).unwrap();
        assert!(
            !detect(&temp.0, None, None, None).nvidia,
            "desktop card not on nvidia"
        );
        std::os::unix::fs::symlink("nvidia", temp.0.join("sys/class/drm/card1/device/driver"))
            .unwrap();
        assert!(detect(&temp.0, None, None, None).nvidia);
        std::fs::remove_file(temp.0.join("proc/driver/nvidia/version")).unwrap();
        assert!(!detect(&temp.0, None, None, None).nvidia, "no proc version");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn the_marker_round_trips_privately_and_clears() {
        let temp = Temp::new("marker");
        let path = temp.0.join("nested").join(MARKER_FILE);
        assert_eq!(read_marker(&path), None);
        write_marker(&path, &marker("615.78.08")).unwrap();
        assert_eq!(read_marker(&path), Some(marker("615.78.08")));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1,
            "no temporary left"
        );
        // The first frame clears it once; a second clear finds nothing and does nothing.
        let slot = Mutex::new(Some(path.clone()));
        assert!(clear_marker(&slot));
        assert_eq!(read_marker(&path), None);
        write_marker(&path, &marker("615.78.08")).unwrap();
        assert!(!clear_marker(&slot), "second clear is a no-op");
        assert_eq!(read_marker(&path), Some(marker("615.78.08")));
        assert!(!clear_marker(&Mutex::new(None)));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn driver_version_comes_from_the_first_line() {
        assert_eq!(
            driver_version("NVRM version: x  570.1.2  Release\n").as_deref(),
            Some("570.1.2")
        );
        assert_eq!(driver_version("odd line\n").as_deref(), Some("odd line"));
        assert_eq!(driver_version(""), None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn the_identifier_matches_the_tauri_configuration() {
        let config =
            std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tauri.conf.json")).unwrap();
        let config: serde_json::Value = serde_json::from_slice(&config).unwrap();
        assert_eq!(config["identifier"], IDENTIFIER);
    }
}
