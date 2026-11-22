//! Windows launch parity: choose the staged session-host CLI only once this
//! process owns the instance; login remains explicit and per-user.
use super::wide;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows_sys::Win32::{
    Foundation::*,
    System::{Recovery::*, Registry::*},
    UI::Shell::SetCurrentProcessExplicitAppUserModelID,
};
use zerus_client_core::actions::{Exit, Invocation};

pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "hgs-tray";

fn session_host(hgs: &Path, action: &str) -> Result<Value, String> {
    let invocation = Invocation {
        args: vec!["session-host".into(), action.into(), "--json".into()],
        stdin: None,
        timeout: Duration::from_secs(30),
        request_id: None,
    };
    match zerus_client_core::run_limited(hgs, &invocation, &[], 64 * 1024) {
        Ok(Exit::Code {
            code: 0, stdout, ..
        }) => serde_json::from_slice(&stdout).map_err(|e| e.to_string()),
        Ok(Exit::Code { stderr, .. }) => Err(stderr),
        _ => Err(format!("hgs session-host {action} did not finish")),
    }
}
fn digest(path: &Path) -> String {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut hash = Sha256::new();
    let mut bytes = 0;
    let mut reader = file.take(256 * 1024 * 1024 + 1);
    let mut block = [0; 64 * 1024];
    loop {
        let Ok(n) = reader.read(&mut block) else {
            return String::new();
        };
        if n == 0 {
            break;
        }
        bytes += n;
        if bytes > 256 * 1024 * 1024 {
            return String::new();
        }
        hash.update(&block[..n]);
    }
    format!("{:x}", hash.finalize())
}

/// Owner, 2026-11-16 (build 205): a running GUI sets ZERUS_CLIENT_HGS to the
/// CLI it chose (the staged `~/.local/bin/hgs.exe`, or the one beside it) for
/// its own children, and an older build's update helper passed that on to the
/// new build. Taken as a chosen CLI it skipped the session host install and
/// this app's binding, so the takeover failed and the update was rolled back.
/// Those are the CLIs this start would pick anyway: a value naming one of them
/// chooses nothing and the normal checks run.
fn inherited_default(value: &Path, defaults: &[&Path]) -> bool {
    let Ok(value) = std::fs::canonicalize(value) else { return false };
    defaults.iter().any(|path| std::fs::canonicalize(path).is_ok_and(|path| path == value))
}

pub fn select_hgs(configured: Option<&Path>, installed: bool) -> PathBuf {
    let executable = std::env::current_exe().ok();
    let adjacent = executable
        .as_ref()
        .and_then(|p| p.parent())
        .map(|p| p.join("hgs.exe"))
        .filter(|p| p.is_file());
    let original = configured.map(Path::to_path_buf).or_else(|| {
        std::env::var_os("ZERUS_CLIENT_HGS")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .filter(|p| {
                let staged = zerus_client_core::default_hgs();
                let defaults: Vec<&Path> = adjacent.as_deref().into_iter().chain([staged.as_path()]).collect();
                !inherited_default(p, &defaults)
            })
    });
    let selected = original
        .clone()
        .or(adjacent)
        .unwrap_or_else(zerus_client_core::default_hgs);
    if !installed || original.is_some() {
        return selected;
    }
    // Shown while a slow check runs, as there is no tray icon yet (zerus-5wmh.13.14.4).
    let note = crate::startup_note::StartupNote::start(
        crate::startup_note::DELAY,
        crate::startup_note::Window::new,
    );
    note.step(crate::native_text::render(&crate::startup_note::text(
        "status",
    )));
    let status = match session_host(&selected, "status") {
        Ok(value) => value,
        Err(error) => {
            eprintln!("zerus-client: session-host status: {error}");
            return selected;
        }
    };
    let digest = if status["enabled"] == true {
        digest(&selected)
    } else {
        String::new()
    };
    // This app's own image, for its binding to the staged CLI (zerus-5wmh.66.8.1.14).
    let app = executable
        .as_deref()
        .and_then(|exe| std::fs::canonicalize(exe).ok());
    let choice =
        crate::windows_shell::choose_hgs(&status, &selected, &digest, None, app.as_deref());
    if choice.refresh {
        note.step(crate::native_text::render(&crate::startup_note::text(
            "install",
        )));
        match session_host(&selected, "install") {
            Ok(installed) => {
                // Task #53 (owner, 2026-11-10, build 175): the install recorded
                // no binding for this app; its fixed code goes to hgs-tray.log
                // and names the step the window shows (never the CLI's text).
                if let Some(code) = installed["frontend_refusal_code"].as_str().and_then(crate::takeover_refusal::record_code) {
                    crate::startup_log::record("failed", "frontend_record_refused", Some(code));
                    crate::takeover_refusal::note_record_refusal(code);
                }
            }
            Err(error) => {
                eprintln!("zerus-client: session-host install: {error}");
                crate::startup_log::record("failed", "session_host_install_failed", None);
                return choice.fallback;
            }
        }
    }
    choice.path
}

pub fn prepare(args: &[String], id: &str) {
    // This process only: keep taskbar grouping and tray notices under the app ID.
    unsafe {
        SetCurrentProcessExplicitAppUserModelID(wide(id).as_ptr());
    }
    let args = crate::windows_shell::restart_arguments(args);
    let mut line = zerus_client_core::shell::windows_program_line("", &args)
        .trim_start()
        .to_owned();
    if line.encode_utf16().count() >= 1024 {
        line = "--background".into();
    }
    let status = unsafe {
        RegisterApplicationRestart(wide(&line).as_ptr(), RESTART_NO_PATCH | RESTART_NO_REBOOT)
    };
    if status < 0 {
        eprintln!("zerus-client: cannot register Windows crash restart: {status:#x}");
    }
}

/// The explicit install/remove commands are also used by the per-user installer.
/// No launch invokes this automatically. Existing Startup shortcuts still work.
pub fn login_item(enabled: bool, executable: &Path, args: &[String]) -> Result<(), String> {
    let command = login_command(executable, args);
    set_login_item(RUN_KEY, RUN_VALUE, enabled.then_some(command.as_str()))
}
fn login_command(executable: &Path, args: &[String]) -> String {
    zerus_client_core::shell::windows_program_line(
        &format!("\"{}\"", executable.to_string_lossy()),
        &crate::windows_shell::restart_arguments(args),
    )
}
/// WhiteWh fork: light mode's own Run value, beside the desktop's.
const LIGHT_VALUE: &str = "hgs-tray light";

/// WhiteWh fork: `"<app>" --light` at sign-in, or no such entry.
pub fn light_login_item(enabled: bool, executable: &Path) -> Result<(), String> {
    let command = light_command(executable);
    set_login_item(RUN_KEY, LIGHT_VALUE, enabled.then_some(command.as_str()))
}
pub fn light_command(executable: &Path) -> String {
    format!("\"{}\" {}", executable.to_string_lossy(), super::light::FLAG)
}
/// WhiteWh fork: light mode's Run value, as `read_login_item` reads the desktop's.
pub fn read_light_login_item() -> Result<Option<String>, crate::login_item::Unreadable> {
    read_value(RUN_KEY, LIGHT_VALUE)
}
/// Reads this app's own Run value only; absent is `None`, and anything that
/// cannot be read as text, or is implausibly long, is unreadable.
pub fn read_login_item() -> Result<Option<String>, crate::login_item::Unreadable> {
    read_value(RUN_KEY, RUN_VALUE)
}
fn read_value(key: &str, value: &str) -> Result<Option<String>, crate::login_item::Unreadable> {
    let mut text = vec![0u16; 16 * 1024];
    let mut size = (text.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(key).as_ptr(),
            wide(value).as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            text.as_mut_ptr().cast(),
            &mut size,
        )
    };
    match status {
        ERROR_SUCCESS => {
            let units = (size as usize / 2).min(text.len());
            let end = text[..units]
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(units);
            String::from_utf16(&text[..end])
                .map(Some)
                .map_err(|_| crate::login_item::Unreadable)
        }
        ERROR_FILE_NOT_FOUND => Ok(None),
        _ => Err(crate::login_item::Unreadable),
    }
}
fn set_login_item(key: &str, value: &str, command: Option<&str>) -> Result<(), String> {
    let key = wide(key);
    let value = wide(value);
    let mut handle = std::ptr::null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut handle,
            std::ptr::null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(format!(
            "Cannot open the per-user login registration ({status})"
        ));
    }
    let status = if let Some(command) = command {
        let text = wide(command);
        unsafe {
            RegSetValueExW(
                handle,
                value.as_ptr(),
                0,
                REG_SZ,
                text.as_ptr().cast(),
                (text.len() * 2) as u32,
            )
        }
    } else {
        unsafe { RegDeleteValueW(handle, value.as_ptr()) }
    };
    unsafe {
        RegCloseKey(handle);
    }
    if status == ERROR_SUCCESS || (command.is_none() && status == ERROR_FILE_NOT_FOUND) {
        Ok(())
    } else {
        Err(format!(
            "Cannot change the per-user login registration ({status})"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_inherited_default_cli_chooses_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let beside = dir.path().join("hgs.exe");
        let other = dir.path().join("other").join("hgs.exe");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&beside, b"cli").unwrap();
        std::fs::write(&other, b"cli").unwrap();
        assert!(inherited_default(&beside, &[&beside]));
        assert!(inherited_default(&other, &[&beside, &other]));
        assert!(!inherited_default(&other, &[&beside]));
        assert!(!inherited_default(&beside, &[]));
    }
    #[test]
    fn restart_line_does_not_inject_an_empty_program_argument() {
        let args = crate::windows_shell::restart_arguments(&[
            "--sessions".into(),
            "--hgs".into(),
            "C:/Program Files/Zerus/hgs.exe".into(),
        ]);
        assert_eq!(
            zerus_client_core::shell::windows_program_line("", &args)
                .trim_start()
                .to_owned(),
            "--background --hgs \"C:/Program Files/Zerus/hgs.exe\""
        );
    }
    #[test]
    fn the_command_login_item_writes_is_a_confirmed_registration() {
        let exe = Path::new(r"C:\Program Files\Zerus\hgs-tray.exe");
        for options in [vec![], vec!["--hgs".to_owned(), r"C:\z\hgs.exe".to_owned()]] {
            let command = login_command(exe, &options);
            assert_eq!(
                crate::login_item::registration(Ok(Some(&command)), exe),
                crate::login_item::Registration::On,
                "{command}"
            );
        }
    }
    #[test]
    fn reading_the_login_value_tells_absent_from_text_from_unreadable() {
        use crate::login_item::Unreadable;
        let key = format!(r"Software\Zerus\Tests\{}", uuid::Uuid::new_v4());
        assert_eq!(read_value(&key, "Zerus"), Ok(None), "absent key");
        set_login_item(&key, "other", Some("keep")).unwrap();
        assert_eq!(read_value(&key, "Zerus"), Ok(None), "absent value");
        set_login_item(&key, "Zerus", Some(r#""C:\Z\hgs-tray.exe" --background"#)).unwrap();
        assert_eq!(
            read_value(&key, "Zerus"),
            Ok(Some(r#""C:\Z\hgs-tray.exe" --background"#.to_owned()))
        );
        // A value of another type is unreadable, never off.
        let mut handle = std::ptr::null_mut();
        let opened = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide(&key).as_ptr(),
                0,
                KEY_SET_VALUE,
                &mut handle,
            )
        };
        assert_eq!(opened, ERROR_SUCCESS);
        let number = 1u32.to_ne_bytes();
        let set = unsafe {
            RegSetValueExW(
                handle,
                wide("Zerus").as_ptr(),
                0,
                REG_DWORD,
                number.as_ptr(),
                4,
            )
        };
        unsafe { RegCloseKey(handle) };
        assert_eq!(set, ERROR_SUCCESS);
        assert_eq!(read_value(&key, "Zerus"), Err(Unreadable));
        assert_eq!(
            unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide(&key).as_ptr()) },
            ERROR_SUCCESS
        );
    }
    #[test]
    fn explicit_login_registration_only_touches_its_selected_value() {
        let key = format!(r"Software\Zerus\Tests\{}", uuid::Uuid::new_v4());
        set_login_item(&key, "other", Some("keep")).unwrap();
        set_login_item(
            &key,
            "Zerus",
            Some(r#""C:\Program Files\Zerus\hgs-tray.exe" --background"#),
        )
        .unwrap();
        set_login_item(&key, "Zerus", None).unwrap();
        // Deleting an absent own value is idempotent, not an error or re-enable.
        set_login_item(&key, "Zerus", None).unwrap();
        let mut size = 0;
        assert_eq!(
            unsafe {
                RegGetValueW(
                    HKEY_CURRENT_USER,
                    wide(&key).as_ptr(),
                    wide("other").as_ptr(),
                    RRF_RT_REG_SZ,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut size,
                )
            },
            ERROR_SUCCESS
        );
        assert_eq!(
            unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide(&key).as_ptr()) },
            ERROR_SUCCESS
        );
    }
}
