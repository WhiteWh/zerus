#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Manager};
use zerus_client_core::actions::{Exit, Invocation};

/// Windows: what the app's own HKCU Run value means for this image. Absent is
/// off; unreadable is unknown, never off; anything but this image started in
/// the background is not confirmed. A Startup-folder shortcut is not seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum Registration {
    On,
    Off,
    Other,
    Unknown,
}

/// The Run value could not be read as text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct Unreadable;

#[cfg_attr(not(windows), allow(dead_code))]
pub fn registration(read: Result<Option<&str>, Unreadable>, executable: &Path) -> Registration {
    let command = match read {
        Err(Unreadable) => return Registration::Unknown,
        Ok(None) => return Registration::Off,
        Ok(Some(command)) => command,
    };
    // The program as `login_item` writes it; Windows paths ignore case. The rest
    // must be what this app's own command line runs as a background start, so
    // help, version, login commands, conflicts and unknown options aren't one.
    let program = format!("\"{}\"", executable.to_string_lossy()).replace('/', "\\");
    let confirmed = command
        .get(..program.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(&program))
        && command[program.len()..].starts_with([' ', '\t'])
        && zerus_client_core::shell::windows_split_arguments(&command[program.len()..])
            .is_some_and(|arguments| {
                matches!(
                    crate::cli::parse(&arguments),
                    Ok(crate::cli::Command::Run(options)) if options.background
                )
            });
    if confirmed {
        Registration::On
    } else {
        Registration::Other
    }
}

/// The actual installed Windows app, outside a replay.
fn registration_allowed(windows: bool, installed: bool, replay: bool) -> bool {
    windows && installed && !replay
}

fn allowed(app: &AppHandle) -> bool {
    registration_allowed(
        cfg!(windows),
        app.state::<crate::identity::Identity>().is_switch(),
        crate::actions::blocked(),
    )
}

#[cfg(windows)]
fn read_registration() -> Registration {
    let Ok(executable) = std::env::current_exe() else {
        return Registration::Unknown;
    };
    let read = crate::platform::windows::startup::read_login_item();
    registration(
        read.as_ref().map(Option::as_deref).map_err(|e| *e),
        &executable,
    )
}

#[cfg(not(windows))]
fn read_registration() -> Registration {
    Registration::Unknown
}

/// The native executable and fixed background arguments: the page sends only on or off.
#[cfg(windows)]
fn write_registration(enabled: bool) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    crate::platform::windows::startup::login_item(enabled, &executable, &[])
}

#[cfg(not(windows))]
fn write_registration(_: bool) -> Result<(), String> {
    Err("Start Zerus at login is available on Windows only.".into())
}

/// WhiteWh fork: Settings > Startup > light mode at sign-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct LightEntry {
    pub available: bool,
    pub on: bool,
}

#[cfg(windows)]
fn read_light() -> bool {
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let expected = crate::platform::windows::startup::light_command(&executable);
    matches!(crate::platform::windows::startup::read_light_login_item(), Ok(Some(command)) if command.eq_ignore_ascii_case(&expected))
}

#[cfg(not(windows))]
fn read_light() -> bool {
    false
}

/// Reads only.
#[tauri::command(async)]
pub fn light_entry_view(app: AppHandle) -> LightEntry {
    let available = allowed(&app);
    LightEntry { available, on: available && read_light() }
}

/// On: light mode starts at sign-in instead of the desktop, whose own entry
/// goes. Off: only light mode's entry goes.
#[tauri::command(async)]
pub fn light_entry_set(app: AppHandle, enabled: bool) -> Result<LightEntry, String> {
    if !allowed(&app) {
        return Err("Light mode at sign-in is not available in this build.".into());
    }
    #[cfg(windows)]
    {
        if enabled {
            write_registration(false)?;
        }
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        crate::platform::windows::startup::light_login_item(enabled, &executable)?;
    }
    Ok(LightEntry { available: true, on: read_light() })
}

/// Starts light mode now; the page then quits the desktop as its Quit does.
#[tauri::command(async)]
pub fn light_mode_start() -> Result<(), String> {
    #[cfg(all(windows, feature = "live"))]
    {
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        std::process::Command::new(executable)
            .arg(crate::LIGHT_FLAG)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|error| error.to_string())?;
        Ok(())
    }
    #[cfg(not(all(windows, feature = "live")))]
    Err("Light mode is available on Windows only.".into())
}

/// What the Settings toggle shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct LoginEntry {
    pub available: bool,
    pub state: Registration,
}

/// Reads only; it never enables, removes or repairs the value.
#[tauri::command(async)]
pub fn login_entry_view(app: AppHandle) -> LoginEntry {
    let available = allowed(&app);
    LoginEntry {
        available,
        state: if available {
            read_registration()
        } else {
            Registration::Unknown
        },
    }
}

/// The person's explicit on or off in Settings.
#[tauri::command(async)]
pub fn login_entry_set(app: AppHandle, enabled: bool) -> Result<LoginEntry, String> {
    if !allowed(&app) {
        return Err("Start Zerus at login is not available in this build.".into());
    }
    write_registration(enabled)?;
    Ok(LoginEntry {
        available: true,
        state: read_registration(),
    })
}

pub fn launched_deprioritized(max_priority: i32, service: &str) -> bool {
    service == "com.hgdev.hgs-tray" && (0..31).contains(&max_priority)
}

#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn thread_max_priority() -> i32 {
    unsafe extern "C" {
        fn mach_port_deallocate(
            task: libc::mach_port_t,
            name: libc::mach_port_t,
        ) -> libc::kern_return_t;
    }
    // SAFETY: the info buffer has the exact type and count thread_info expects;
    // mach_thread_self's send right is released before returning.
    unsafe {
        let mut info: libc::thread_extended_info_data_t = std::mem::zeroed();
        let mut count = libc::THREAD_EXTENDED_INFO_COUNT;
        let thread = libc::mach_thread_self();
        let result = libc::thread_info(
            thread,
            libc::THREAD_EXTENDED_INFO as libc::thread_flavor_t,
            &mut info as *mut _ as libc::thread_info_t,
            &mut count,
        );
        let _ = mach_port_deallocate(libc::mach_task_self(), thread);
        if result == libc::KERN_SUCCESS {
            info.pth_maxpriority
        } else {
            -1
        }
    }
}

/// Checked once at startup on the main thread; App Nap may change it later.
pub fn deprioritized() -> bool {
    #[cfg(target_os = "macos")]
    {
        launched_deprioritized(
            thread_max_priority(),
            &std::env::var("XPC_SERVICE_NAME").unwrap_or_default(),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

fn setup(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    let setup = contents.join("Resources/bin/zerus-setup");
    let metadata = setup.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    Some(setup)
}

pub fn fix(executable: &Path) -> Result<(), String> {
    let setup = setup(executable).ok_or_else(|| {
        "This Zerus cannot update its login item itself. For a source installation, run packaging/desktop/install.sh again; the change applies at your next login.".to_owned()
    })?;
    let invocation = Invocation {
        args: vec!["--autostart".into()],
        stdin: None,
        timeout: Duration::from_secs(60),
        request_id: None,
    };
    match zerus_client_core::run_limited(&setup, &invocation, &[], 32 * 1024) {
        Ok(Exit::Code { code: 0, .. }) => Ok(()),
        Ok(Exit::Code { stdout, stderr, .. }) => {
            let stdout = String::from_utf8_lossy(&stdout);
            let detail = [stdout.trim(), stderr.trim()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if detail.is_empty() {
                Err("The login item could not be updated.".into())
            } else {
                Err(detail)
            }
        }
        Ok(Exit::StartFailed(detail)) if !detail.is_empty() => Err(detail),
        _ => Err("The login item could not be updated.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_installed_app_outside_a_replay_on_windows_changes_its_login_entry() {
        assert!(registration_allowed(true, true, false));
        for (windows, installed, replay) in [
            (false, true, false),
            (true, false, false),
            (true, true, true),
        ] {
            assert!(!registration_allowed(windows, installed, replay));
        }
    }

    /// The command `platform::windows::startup::login_command` builds.
    fn login_command(executable: &Path, options: &[String]) -> String {
        zerus_client_core::shell::windows_program_line(
            &format!("\"{}\"", executable.to_string_lossy()),
            &crate::windows_shell::restart_arguments(options),
        )
    }

    #[test]
    fn a_login_command_for_this_image_is_the_only_confirmed_registration() {
        let exe = Path::new(r"C:\Program Files\Zerus\hgs-tray.exe");
        let own = login_command(exe, &[]);
        assert_eq!(
            own,
            r#""C:\Program Files\Zerus\hgs-tray.exe"  --background"#
        );
        assert_eq!(registration(Ok(Some(&own)), exe), Registration::On);
        let by_hand = r#""C:\Program Files\Zerus\hgs-tray.exe" --background"#;
        assert_eq!(registration(Ok(Some(by_hand)), exe), Registration::On);
        // What `--install-login-item --hgs X` wrote is still this image's entry.
        let selected = login_command(exe, &["--hgs".into(), r"C:\z\hgs.exe".into()]);
        assert_eq!(registration(Ok(Some(&selected)), exe), Registration::On);
        // Windows paths ignore case.
        let shouted = r#""C:\PROGRAM FILES\ZERUS\HGS-TRAY.EXE" --background"#;
        assert_eq!(registration(Ok(Some(shouted)), exe), Registration::On);
        // Every option the current command line accepts, in the order the person wrote.
        for command in [
            r#""C:\Program Files\Zerus\hgs-tray.exe" --background --hgs "C:\Program Files\z\hgs.exe""#,
            r#""C:\Program Files\Zerus\hgs-tray.exe" --background --open-mode tabs --local-poll 5"#,
            r#""C:\Program Files\Zerus\hgs-tray.exe" --hgs C:\z\hgs.exe --background"#,
            r#""C:\Program Files\Zerus\hgs-tray.exe"	--background"#,
        ] {
            assert_eq!(
                registration(Ok(Some(command)), exe),
                Registration::On,
                "{command}"
            );
        }
    }

    #[test]
    fn a_login_entry_that_is_absent_is_off_and_an_unreadable_one_is_unknown() {
        let exe = Path::new(r"C:\Zerus\hgs-tray.exe");
        assert_eq!(registration(Ok(None), exe), Registration::Off);
        assert_eq!(registration(Err(Unreadable), exe), Registration::Unknown);
    }

    #[test]
    fn a_different_or_malformed_login_entry_is_not_confirmed() {
        let exe = Path::new(r"C:\Zerus\hgs-tray.exe");
        for command in [
            "",
            " ",
            r#""C:\Other\hgs-tray.exe" --background"#,
            r"C:\Zerus\hgs-tray.exe --background",
            r#""C:\Zerus\hgs-tray.exe""#,
            r#""C:\Zerus\hgs-tray.exe" --sessions"#,
            r#""C:\Zerus\hgs-tray.exe" --backgroundx"#,
            r#""C:\Zerus\hgs-tray.exe" --BACKGROUND"#,
            // This image, but the rest of the command doesn't start Zerus in the background.
            r#""C:\Zerus\hgs-tray.exe" --background --help"#,
            r#""C:\Zerus\hgs-tray.exe" --background -h"#,
            r#""C:\Zerus\hgs-tray.exe" --background --version"#,
            r#""C:\Zerus\hgs-tray.exe" --background --build-info"#,
            r#""C:\Zerus\hgs-tray.exe" --background --sessions"#,
            r#""C:\Zerus\hgs-tray.exe" --background --install-login-item"#,
            r#""C:\Zerus\hgs-tray.exe" --background --remove-login-item"#,
            r#""C:\Zerus\hgs-tray.exe" --background --selftest"#,
            r#""C:\Zerus\hgs-tray.exe" --background --frobnicate"#,
            r#""C:\Zerus\hgs-tray.exe" --background --hgs"#,
            r#""C:\Zerus\hgs-tray.exe" --background --hgs "C:\no end"#,
            r#""C:\Zerus\hgs-tray.exe" --hgs --background"#,
            r#""C:\Zerus\hgs-tray.exe" --hgs C:\z\hgs.exe"#,
            r#""C:\Zerus\hgs-tray.exe"--background"#,
            r#"cmd /c "C:\Zerus\hgs-tray.exe" --background"#,
            "é",
        ] {
            assert_eq!(
                registration(Ok(Some(command)), exe),
                Registration::Other,
                "{command:?}"
            );
        }
    }

    #[test]
    fn the_login_state_reaches_the_page_by_name() {
        for (state, name) in [
            (Registration::On, "on"),
            (Registration::Off, "off"),
            (Registration::Other, "other"),
            (Registration::Unknown, "unknown"),
        ] {
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                serde_json::json!(name)
            );
        }
    }

    #[test]
    fn only_the_known_clamped_login_item_is_deprioritized() {
        assert!(launched_deprioritized(20, "com.hgdev.hgs-tray"));
        assert!(launched_deprioritized(0, "com.hgdev.hgs-tray"));
        for (priority, service) in [
            (-1, "com.hgdev.hgs-tray"),
            (31, "com.hgdev.hgs-tray"),
            (20, "another.service"),
            (20, ""),
        ] {
            assert!(!launched_deprioritized(priority, service));
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_fix_runs_only_the_bundled_setup_with_autostart() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        let environment = zerus_client_core::run::FrozenEnvironment::capture().unwrap();
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "zerus-login-item-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let executable = root.join("Contents/MacOS/zerus-client");
        let setup = root.join("Contents/Resources/bin/zerus-setup");
        std::fs::create_dir_all(setup.parent().unwrap()).unwrap();
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, "").unwrap();
        // Configure only this fake setup file. Mutating the parent environment
        // invalidates unrelated termination controllers running in parallel.
        let script = |exit| {
            format!(
            "#!/bin/sh\nprintf '%s' \"$1\" > \"${{0%/*}}/args\"\nprintf 'fixture problem\\n' >&2\nexit {exit}\n"
        )
        };
        crate::write_executable(&setup, script(0));
        let log = setup.with_file_name("args");
        assert!(
            environment.current(),
            "fixture preparation invalidated an unrelated native context"
        );
        fix(&executable).unwrap();
        assert!(
            environment.current(),
            "setup changed the parent environment"
        );
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "--autostart");
        crate::write_executable(&setup, script(7));
        assert_eq!(fix(&executable).unwrap_err(), "fixture problem");
        assert!(
            environment.current(),
            "failed setup changed the parent environment"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
