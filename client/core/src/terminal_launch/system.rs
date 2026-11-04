//! The [`Desktop`] of this computer.

use super::{Desktop, Start};
use std::path::{Path, PathBuf};

/// This computer: PATH, the environment, and detached processes.
#[derive(Debug, Default)]
pub struct System;

impl Desktop for System {
    fn find(&self, program: &str) -> Option<String> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .filter(|folder| !folder.as_os_str().is_empty())
            .flat_map(|folder| candidates(&folder, program))
            .find(|candidate| executable(candidate))
            .map(|found| found.to_string_lossy().into_owned())
    }

    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|value| !value.is_empty())
    }

    fn exists(&self, path: &str) -> bool {
        // An app execution alias is a reparse point that metadata cannot follow.
        std::fs::symlink_metadata(path).is_ok()
    }

    fn home(&self) -> String {
        std::env::home_dir()
            .map(|home| home.to_string_lossy().into_owned())
            .unwrap_or_else(|| if cfg!(windows) { "C:\\" } else { "/" }.to_owned())
    }

    fn console_host(&self) -> String {
        std::env::current_exe()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn start(&mut self, start: &Start) -> Result<(), String> {
        start_detached(start)
    }
}

/// The names `program` may have in `folder`: on Windows with each PATHEXT
/// extension, then as given when it has one.
fn candidates(folder: &Path, program: &str) -> Vec<PathBuf> {
    if !cfg!(windows) {
        return vec![folder.join(program)];
    }
    let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
    let mut names: Vec<PathBuf> = extensions
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(|extension| folder.join(format!("{program}{}", extension.to_ascii_lowercase())))
        .collect();
    if Path::new(program).extension().is_some() {
        names.push(folder.join(program));
    }
    names
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn executable(path: &Path) -> bool {
    // wt.exe is an app execution alias, which metadata cannot follow.
    std::fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_dir())
}

/// Starts the program in a session of its own, as `QProcess::startDetached`
/// does, keeping only standard input, output and error. A thread reaps it
/// when it ends, so it never lingers as a zombie.
#[cfg(unix)]
fn start_detached(start: &Start) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let mut command = crate::child_environment::command(&start.program);
    command.args(&start.arguments).current_dir(&start.directory);
    // SAFETY: the hook runs between fork and exec and only makes
    // async-signal-safe system calls.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            keep_only_stdio();
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let _ = std::thread::Builder::new()
        .name("terminal-reaper".to_owned())
        .stack_size(64 * 1024)
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
}

/// Marks every descriptor above standard error close-on-exec. Closing them
/// here instead would also close the pipe that reports a failed exec.
#[cfg(unix)]
fn keep_only_stdio() {
    // One call on Linux 5.11 and later; the loop elsewhere.
    #[cfg(target_os = "linux")]
    // SAFETY: close_range takes no pointers.
    let marked = unsafe {
        const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;
        libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, CLOSE_RANGE_CLOEXEC) == 0
    };
    #[cfg(not(target_os = "linux"))]
    let marked = false;
    if marked {
        return;
    }
    // SAFETY: getrlimit and fcntl take no pointers beyond the local limit.
    unsafe {
        let mut limit: libc::rlimit = std::mem::zeroed();
        let max = if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0 {
            limit.rlim_cur.min(65_536) as libc::c_int
        } else {
            1024
        };
        for fd in 3..max {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
}

/// Starts the program as `QProcess::startDetached` with the Qt GUI's console
/// rules (`WindowsConsole::show`): a console program gets a new console
/// instead of the standard handles of a GUI process, which has none; the
/// console host is shown normally. No handle is inherited.
#[cfg(windows)]
fn start_detached(start: &Start) -> Result<(), String> {
    use super::Window;
    use crate::shell::windows_program_line;
    use std::ptr::null;
    use windows_sys::Win32::Foundation::{CloseHandle, FALSE};
    use windows_sys::Win32::System::Console::GetConsoleWindow;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, CREATE_NEW_CONSOLE, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT,
        PROCESS_INFORMATION, STARTF_USESHOWWINDOW, STARTUPINFOW,
    };
    const SW_SHOWNORMAL: u16 = 1;

    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(std::iter::once(0)).collect() };
    let mut line = wide(&windows_program_line(&start.program, &start.arguments));
    let directory = wide(&start.directory);
    // SAFETY: zeroed STARTUPINFOW and PROCESS_INFORMATION are valid; every
    // buffer is NUL-terminated and outlives the call.
    unsafe {
        let mut startup: STARTUPINFOW = std::mem::zeroed();
        startup.cb = size_of::<STARTUPINFOW>() as u32;
        let mut flags = CREATE_UNICODE_ENVIRONMENT;
        match start.window {
            Window::Plain => {
                if GetConsoleWindow().is_null() {
                    flags |= CREATE_NO_WINDOW;
                }
            }
            Window::Console => flags |= CREATE_NEW_CONSOLE,
            Window::Shown => {
                startup.dwFlags |= STARTF_USESHOWWINDOW;
                startup.wShowWindow = SW_SHOWNORMAL;
            }
        }
        let mut info: PROCESS_INFORMATION = std::mem::zeroed();
        if CreateProcessW(
            null(),
            line.as_mut_ptr(),
            null(),
            null(),
            FALSE,
            flags,
            null(),
            directory.as_ptr(),
            &startup,
            &mut info,
        ) == 0
        {
            let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
            return Err(super::windows_start_error(
                code,
                format_message(code, LANGUAGE_DEFAULT).as_deref(),
            ));
        }
        CloseHandle(info.hThread);
        CloseHandle(info.hProcess);
    }
    Ok(())
}

/// `MAKELANGID(LANG_NEUTRAL, SUBLANG_DEFAULT)`: the user's language.
#[cfg(windows)]
pub(super) const LANGUAGE_DEFAULT: u32 = 0x0400;
/// `MAKELANGID(LANG_ENGLISH, SUBLANG_ENGLISH_US)`.
#[cfg(windows)]
pub(super) const LANGUAGE_ENGLISH: u32 = 0x0409;

/// The system's text for an error code in `language`; None when it has none.
#[cfg(windows)]
pub(super) fn format_message(code: u32, language: u32) -> Option<String> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::System::Diagnostics::Debug::{
        FormatMessageW, FORMAT_MESSAGE_ALLOCATE_BUFFER, FORMAT_MESSAGE_FROM_SYSTEM,
        FORMAT_MESSAGE_IGNORE_INSERTS,
    };
    let mut buffer: *mut u16 = null_mut();
    // SAFETY: with ALLOCATE_BUFFER the system stores a LocalAlloc'd buffer of
    // `length` units in `buffer`, which is read once and freed once.
    unsafe {
        let length = FormatMessageW(
            FORMAT_MESSAGE_ALLOCATE_BUFFER
                | FORMAT_MESSAGE_FROM_SYSTEM
                | FORMAT_MESSAGE_IGNORE_INSERTS,
            null(),
            code,
            language,
            (&mut buffer as *mut *mut u16).cast(),
            0,
            null(),
        );
        if buffer.is_null() {
            return None;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(buffer, length as usize));
        LocalFree(buffer.cast());
        Some(text)
    }
}
