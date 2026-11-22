//! WhiteWh fork: light mode, `hgs-tray.exe --light`. A notification-area icon
//! and nothing else: no window, webview, GPU context or desktop engine. It
//! starts `hgs session-host keep` (the keeper of the session host and the team
//! worker, `src/host/keeper.rs`), shows in its tooltip whether the session
//! host runs, checks once a minute and starts the keeper again if it left.
//! Open Zerus starts the full desktop and closes the light icon; Quit closes
//! only the icon, and the keeper stays.
use super::{drawing, wide};
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;
use windows_sys::Win32::{
    Foundation::*,
    System::LibraryLoader::GetModuleHandleW,
    System::Threading::CreateMutexW,
    UI::{Shell::*, WindowsAndMessaging::*},
};
use zerus_client_core::{menubar::Face, text, text::Text};

/// The command-line flag.
pub const FLAG: &str = "--light";
const CALLBACK: u32 = WM_APP + 51;
/// The status thread has a new tooltip.
const UPDATED: u32 = WM_APP + 52;
const ICON: u32 = 1;
const OPEN: usize = 1;
const QUIT: usize = 2;
const CHECK: Duration = Duration::from_secs(60);
/// Hide the console window (CREATE_NO_WINDOW).
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static WINDOW: AtomicUsize = AtomicUsize::new(0);
static TOOLTIP: Mutex<String> = Mutex::new(String::new());
static TASKBAR_CREATED: AtomicUsize = AtomicUsize::new(0);

/// Runs the light icon until Quit or Open Zerus; the exit code.
pub fn run() -> i32 {
    // One light icon per user session.
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, wide("Local\\ZerusLightTray").as_ptr()) };
    if mutex.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return 0;
    }
    let russian = crate::system_language::read().to_ascii_lowercase().starts_with("ru");
    crate::native_text::configure(if russian { "ru" } else { "en" });
    let foreground = zerus_client_core::trayicon::foreground(!super::tray::light_taskbar(), None);
    let Ok(image) = drawing::tile(
        &Face::counts(0, 0, false),
        unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16),
        foreground,
    ) else {
        return 1;
    };
    let class = wide("ZerusLightNotificationArea");
    let native = unsafe { GetModuleHandleW(std::ptr::null()) };
    let descriptor = WNDCLASSW {
        lpfnWndProc: Some(procedure),
        hInstance: native,
        lpszClassName: class.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    if unsafe { RegisterClassW(&descriptor) } == 0 {
        return 1;
    }
    TASKBAR_CREATED.store(
        unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) } as usize,
        Ordering::Release,
    );
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            wide("Zerus").as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            native,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return 1;
    }
    WINDOW.store(hwnd as usize, Ordering::Release);
    *TOOLTIP.lock().unwrap_or_else(PoisonError::into_inner) = tooltip(&[text!("Zerus light mode"), text!("Checking the session host…")]);
    ICON_HANDLE.store(image.handle as usize, Ordering::Release);
    if !notify(hwnd, NIM_ADD) {
        return 1;
    }
    std::thread::spawn(status_loop);
    let mut message: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    drop(image);
    0
}

static ICON_HANDLE: AtomicUsize = AtomicUsize::new(0);

fn tooltip(lines: &[Text]) -> String {
    crate::native_text::lines(lines)
}

fn notify(hwnd: HWND, action: u32) -> bool {
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = ICON;
    if action == NIM_DELETE {
        return unsafe { Shell_NotifyIconW(NIM_DELETE, &data) } != 0;
    }
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.hIcon = ICON_HANDLE.load(Ordering::Acquire) as HICON;
    data.uCallbackMessage = CALLBACK;
    data.szTip = crate::windows_shell::wide_text(&TOOLTIP.lock().unwrap_or_else(PoisonError::into_inner));
    if unsafe { Shell_NotifyIconW(action, &data) } == 0 {
        return false;
    }
    if action == NIM_ADD {
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) };
    }
    true
}

/// The keeper and the host, once a minute, off the window's thread.
fn status_loop() {
    loop {
        let lines = match hgs(&["session-host", "autostart", "status", "--json"]) {
            Ok(status) => {
                if status["keeper"] != true {
                    let _ = hgs(&["session-host", "keep", "--json"]);
                }
                match status["host"].as_str().unwrap_or_default() {
                    "running" => vec![text!("Zerus light mode"), text!("Session host running")],
                    "not_installed" => vec![text!("Zerus light mode"), text!("Local sessions are not enabled on this computer")],
                    _ => vec![text!("Zerus light mode"), text!("Session host not running")],
                }
            }
            Err(error) => vec![text!("Zerus light mode"), Text::verbatim(error)],
        };
        *TOOLTIP.lock().unwrap_or_else(PoisonError::into_inner) = tooltip(&lines);
        let hwnd = WINDOW.load(Ordering::Acquire) as HWND;
        if !hwnd.is_null() {
            unsafe { PostMessageW(hwnd, UPDATED, 0, 0) };
        }
        std::thread::sleep(CHECK);
    }
}

/// One windowless hgs command's JSON answer.
fn hgs(args: &[&str]) -> Result<serde_json::Value, String> {
    let output = Command::new(zerus_client_core::default_hgs())
        .args(args)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(error.trim().trim_start_matches("hgs: ").chars().take(100).collect());
    }
    serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())
}

fn menu(hwnd: HWND) {
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        AppendMenuW(menu, MF_STRING, OPEN, wide(&crate::native_text::render(&text!("Open Zerus"))).as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, QUIT, wide(&crate::native_text::render(&text!("Quit"))).as_ptr());
        let mut point = POINT { x: 0, y: 0 };
        GetCursorPos(&mut point);
        SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, point.x, point.y, 0, hwnd, std::ptr::null());
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        match chosen as usize {
            OPEN => open_full(hwnd),
            QUIT => quit(hwnd),
            _ => {}
        }
    }
}

/// The full desktop, then this icon leaves; the keeper stays.
fn open_full(hwnd: HWND) {
    if let Ok(executable) = std::env::current_exe() {
        let _ = Command::new(executable).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    }
    quit(hwnd);
}

fn quit(hwnd: HWND) {
    notify(hwnd, NIM_DELETE);
    unsafe {
        DestroyWindow(hwnd);
        PostQuitMessage(0);
    }
}

unsafe extern "system" fn procedure(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if message == CALLBACK {
        let event = (l & 0xffff) as u32;
        if event == WM_CONTEXTMENU || event == WM_RBUTTONUP {
            menu(hwnd);
        } else if event == WM_LBUTTONDBLCLK || event == NIN_SELECT || event == (NIN_SELECT | NINF_KEY) {
            open_full(hwnd);
        }
        return 0;
    }
    if message == UPDATED {
        notify(hwnd, NIM_MODIFY);
        return 0;
    }
    if message as usize == TASKBAR_CREATED.load(Ordering::Acquire) {
        // Explorer restarted: the icon comes back.
        notify(hwnd, NIM_ADD);
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, message, w, l) }
}
