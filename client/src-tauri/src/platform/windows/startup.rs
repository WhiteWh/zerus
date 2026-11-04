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
    });
    let selected = original
        .clone()
        .or(adjacent)
        .unwrap_or_else(zerus_client_core::default_hgs);
    if !installed || original.is_some() {
        return selected;
    }
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
    let choice = crate::windows_shell::choose_hgs(&status, &selected, &digest, None);
    if choice.refresh {
        if let Err(error) = session_host(&selected, "install") {
            eprintln!("zerus-client: session-host install: {error}");
            return choice.fallback;
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
    let command = zerus_client_core::shell::windows_program_line(
        &format!("\"{}\"", executable.to_string_lossy()),
        &crate::windows_shell::restart_arguments(args),
    );
    set_login_item(RUN_KEY, RUN_VALUE, enabled.then_some(command.as_str()))
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
