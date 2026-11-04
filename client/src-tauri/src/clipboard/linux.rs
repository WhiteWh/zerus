//! ClipboardBackend.cpp's Klipper -> wl-copy -> native chain. Never runs on
//! GTK's thread. No persistent clipboard store or notification/effect writer.
use std::ffi::OsStr;
use std::fs::File;
use std::io::{Seek, Write};
use std::os::fd::FromRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::AppHandle;
use tauri_plugin_clipboard_manager::ClipboardExt;
use zbus::blocking::{connection::Builder, Proxy};

const TIMEOUT: Duration = Duration::from_secs(3);
// A second copy must not interleave fallbacks with the current copy.
static COPYING: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    Klipper,
    WlCopy,
    Native,
}
fn chain(mut attempt: impl FnMut(Method) -> Result<(), String>) -> Result<(), String> {
    let mut failures = Vec::new();
    for (method, name) in [
        (Method::Klipper, "klipper"),
        (Method::WlCopy, "wl-copy"),
        (Method::Native, "clipboard"),
    ] {
        match attempt(method) {
            Ok(()) => return Ok(()),
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    Err(format!(
        "cannot put the command on the clipboard ({})",
        failures.join("; ")
    ))
}

pub(super) fn write(app: &AppHandle, text: &str) -> Result<(), String> {
    let _copying = COPYING.lock().unwrap_or_else(|error| error.into_inner());
    chain(|method| match method {
        Method::Klipper => klipper(None, text, TIMEOUT),
        Method::WlCopy => wl_copy(
            OsStr::new("wl-copy"),
            &std::env::home_dir().unwrap_or_else(|| "/".into()),
            text,
            TIMEOUT,
        ),
        Method::Native => checked_native(
            text,
            || app.clipboard().write_text(text).map_err(|e| e.to_string()),
            || app.clipboard().read_text().map_err(|e| e.to_string()),
        ),
    })
}

fn checked_native(
    text: &str,
    write: impl FnOnce() -> Result<(), String>,
    read: impl FnOnce() -> Result<String, String>,
) -> Result<(), String> {
    write()?;
    if read()? != text {
        return Err("the clipboard write did not stick".into());
    }
    Ok(())
}

fn klipper(address: Option<&str>, text: &str, timeout: Duration) -> Result<(), String> {
    let builder = match address {
        Some(address) => Builder::address(address),
        None => Builder::session(),
    }
    .map_err(|e| e.to_string())?;
    let bus = builder
        .method_timeout(timeout)
        .build()
        .map_err(|e| e.to_string())?;
    let daemon = Proxy::new(
        &bus,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .map_err(|e| e.to_string())?;
    let owner: String = daemon
        .call("GetNameOwner", &("org.kde.klipper",))
        .map_err(|e| e.to_string())?;
    // A unique name cannot auto-activate a service. Use the timed call path:
    // zbus 5.19 call_with_flags bypasses Connection::method_timeout.
    let proxy = Proxy::new(&bus, owner.as_str(), "/klipper", "org.kde.klipper.klipper")
        .map_err(|e| e.to_string())?;
    // The method's reply is the acknowledgement, as in Qt; no second bus read.
    proxy
        .call::<_, _, ()>("setClipboardContents", &(text,))
        .map_err(|e| e.to_string())
}

fn wl_copy(program: &OsStr, home: &Path, text: &str, timeout: Duration) -> Result<(), String> {
    // An anonymous memory file avoids a blocked pipe writer and keeps private
    // text out of argv and the disk. Leading '-' and trailing newlines survive.
    // SAFETY: memfd_create copies the static NUL-terminated name.
    let fd = unsafe { libc::memfd_create(c"zerus-clipboard".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: fd is a newly owned descriptor, closed by File on every path.
    let mut input = unsafe { File::from_raw_fd(fd) };
    input
        .write_all(text.as_bytes())
        .map_err(|e| e.to_string())?;
    input.rewind().map_err(|e| e.to_string())?;
    let mut command = crate::child_environment::command(program);
    command
        .args(["-t", "text/plain"])
        .current_dir(home)
        .stdin(Stdio::from(input))
        // wl-copy forks the selection owner; inherited pipes would never EOF.
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: the hook only makes an async-signal-safe syscall. Marking (rather
    // than closing) retains Rust's exec-error pipe until exec. If unsupported,
    // refuse this helper and let the native fallback run.
    unsafe {
        command.pre_exec(|| {
            if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 1u32 << 2) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|_| "cannot start wl-copy (not in PATH?)".to_owned())?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!(
                    "wl-copy exited with code {}",
                    status.code().unwrap_or(-1)
                ))
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                // Only our front process. Its forked selection owner must live
                // until the next selection; killing it would erase the copy.
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "wl-copy did not answer in {} ms",
                    timeout.as_millis()
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests;
