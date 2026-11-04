//! Windows' notification-area shell, using Qt's Shell_NotifyIcon behavior.
//! All native state lives on the Tauri main thread. The shared Bar keeps the
//! menu snapshot stable while TrackPopupMenu runs its nested message loop.
use super::{drawing, wide};
use crate::desk::{Bar, DeskInput, Item, MenuAction};
use std::collections::HashMap;
use std::io;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex, PoisonError,
};
use tauri::{AppHandle, Manager};
use windows_sys::Win32::{
    Foundation::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{Shell::*, WindowsAndMessaging::*},
};
use zerus_client_core::{
    attention::Notice,
    menubar::{Apply, Entry, Face, OpenMode},
    text::Text,
};

const CALLBACK: u32 = WM_APP + 41;
const ICON: u32 = 1;
const KEY_SELECT: u32 = NIN_SELECT | NINF_KEY;
pub struct WindowsShell {
    hwnd: AtomicUsize,
    state: AtomicUsize,
}
struct State {
    app: AppHandle,
    // Native shell calls can dispatch nonqueued messages reentrantly while
    // Inner is locked. Ordinary window messages must not acquire that lock.
    taskbar_created: u32,
    inner: Mutex<Inner>,
}
struct Inner {
    bar: Bar,
    menu: HMENU,
    bindings: HashMap<u32, String>,
    icon: drawing::Icon,
    foreground: zerus_client_core::trayicon::Rgb,
    tooltip: String,
    badge: Option<String>,
    current_notice: crate::windows_shell::NoticeClicks,
}
impl Drop for Inner {
    fn drop(&mut self) {
        unsafe {
            if !self.menu.is_null() {
                DestroyMenu(self.menu);
            }
        }
    }
}
fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON,
        ..unsafe { std::mem::zeroed() }
    }
}
fn state(hwnd: HWND) -> Option<&'static State> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const State;
    if pointer.is_null() {
        None
    } else {
        Some(unsafe { &*pointer })
    }
}
fn icon(hwnd: HWND, inner: &Inner, add: bool) -> bool {
    let mut data = data(hwnd);
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.hIcon = inner.icon.handle;
    data.uCallbackMessage = CALLBACK;
    data.szTip = crate::windows_shell::wide_text(&inner.tooltip);
    if unsafe { Shell_NotifyIconW(if add { NIM_ADD } else { NIM_MODIFY }, &data) } == 0 {
        return false;
    }
    if add {
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        unsafe {
            Shell_NotifyIconW(NIM_SETVERSION, &data);
        }
    }
    true
}
unsafe extern "system" fn procedure(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if message == WM_NCCREATE {
        let create = unsafe { &*(l as *const CREATESTRUCTW) };
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
    }
    if message == WM_NCDESTROY {
        let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        }
        // The constructor/managed shell retains the box until CreateWindowEx
        // or DestroyWindow has returned; failure cannot double-free the state.
        let _ = pointer;
        return unsafe { DefWindowProcW(hwnd, message, w, l) };
    }
    if let Some(state) = state(hwnd) {
        if state.taskbar_created != 0 && message == state.taskbar_created {
            let inner = state.inner.lock().unwrap_or_else(PoisonError::into_inner);
            icon(hwnd, &inner, true);
            return 0;
        }
        if message == CALLBACK {
            match (l as u32) & 0xffff {
                WM_LBUTTONUP | NIN_SELECT | KEY_SELECT => {
                    let _ = crate::lifecycle::request_main(&state.app);
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                NIN_BALLOONUSERCLICK => {
                    let token = state
                        .inner
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .current_notice
                        .clicked();
                    if let (Some(token), Some(sender)) = (token, crate::desk::sender(&state.app)) {
                        let _ = sender.send(DeskInput::Clicked(token));
                    }
                }
                _ => {}
            }
            return 0;
        }
    }
    unsafe { DefWindowProcW(hwnd, message, w, l) }
}
fn show_menu(hwnd: HWND) {
    let Some(state) = state(hwnd) else { return };
    let menu = {
        let mut inner = state.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.bar.began();
        inner.menu
    };
    let mut position = POINT::default();
    unsafe {
        GetCursorPos(&mut position);
        SetForegroundWindow(hwnd);
    }
    // No mutex guard or mutable state borrow spans Windows' nested menu loop.
    let chosen = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            position.x,
            position.y,
            0,
            hwnd,
            std::ptr::null(),
        )
    } as u32;
    unsafe {
        PostMessageW(hwnd, WM_NULL, 0, 0);
    }
    let action = {
        let mut inner = state.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let action = inner
            .bindings
            .get(&chosen)
            .and_then(|id| inner.bar.action(id));
        inner.bar.ended();
        let changes = inner.bar.settle();
        apply(hwnd, &mut inner, changes);
        action
    };
    if let Some(action) = action {
        dispatch(&state.app, action);
    }
}
fn dispatch(app: &AppHandle, action: MenuAction) {
    match action {
        MenuAction::Open => {
            let _ = crate::lifecycle::request_main(app);
        }
        MenuAction::Updates => crate::updates::open_page(app),
        MenuAction::Restart => crate::updates::open_page(app),
        MenuAction::Session {
            computer,
            key,
            running: false,
            ..
        } => {
            crate::desk::open_session(app, &computer, &key);
        }
        MenuAction::Session {
            computer,
            name,
            tty,
            ..
        } => {
            let app = app.clone();
            tauri::async_runtime::spawn_blocking(move || {
                crate::desk::open_running(&app, &computer, &name, &tty)
            });
        }
        MenuAction::Mode(mode) => crate::desk::set_mode(app, mode),
        MenuAction::Refresh => {
            let fleet = app.state::<crate::commands::AppState>();
            fleet.refresh();
        }
        MenuAction::Quit => crate::exit_flush::quit_with_flush(app, 0),
    }
}
fn menu(inner: &mut Inner, entries: &[Entry]) {
    let native = unsafe { CreatePopupMenu() };
    if native.is_null() {
        return;
    }
    let mut bindings = HashMap::new();
    let mut next = 1u32;
    for item in inner.bar.layout_modes(entries, &OpenMode::WINDOWS) {
        match item {
            Item::Action { id, text } => {
                unsafe {
                    AppendMenuW(
                        native,
                        MF_STRING,
                        next as usize,
                        wide(&text.replace('&', "&&")).as_ptr(),
                    );
                }
                bindings.insert(next, id);
                next += 1;
            }
            Item::Label { text, .. } => unsafe {
                AppendMenuW(
                    native,
                    MF_STRING | MF_DISABLED,
                    0,
                    wide(&text.replace('&', "&&")).as_ptr(),
                );
            },
            Item::Separator => unsafe {
                AppendMenuW(native, MF_SEPARATOR, 0, std::ptr::null());
            },
            Item::OpenIn { modes, .. } => {
                let submenu = unsafe { CreatePopupMenu() };
                for (id, mode, checked) in modes {
                    let text = match mode {
                        OpenMode::Tab => zerus_client_core::source!("Windows Terminal tab"),
                        OpenMode::Window => zerus_client_core::source!("Windows Terminal window"),
                        _ => mode.label(),
                    };
                    let text = crate::native_text::render(&Text::new(text));
                    unsafe {
                        AppendMenuW(
                            submenu,
                            MF_STRING | if checked { MF_CHECKED } else { MF_UNCHECKED },
                            next as usize,
                            wide(&text.replace('&', "&&")).as_ptr(),
                        );
                    }
                    bindings.insert(next, id);
                    next += 1;
                }
                let label = crate::native_text::render(&Text::new("Open in"));
                unsafe {
                    AppendMenuW(
                        native,
                        MF_POPUP,
                        submenu as usize,
                        wide(&label.replace('&', "&&")).as_ptr(),
                    );
                }
            }
        }
    }
    let old = std::mem::replace(&mut inner.menu, native);
    inner.bindings = bindings;
    if !old.is_null() {
        unsafe {
            DestroyMenu(old);
        }
    }
}
fn apply(hwnd: HWND, inner: &mut Inner, changes: Vec<Apply>) {
    for change in changes {
        match change {
            Apply::Face(face) => {
                match drawing::tile(
                    &face,
                    unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16),
                    inner.foreground,
                ) {
                    Ok(image) => {
                        let old = std::mem::replace(&mut inner.icon, image);
                        icon(hwnd, inner, false);
                        drop(old);
                    }
                    Err(error) => eprintln!("zerus-client: Windows tray icon: {error}"),
                }
            }
            Apply::Tooltip(text) => {
                inner.tooltip = text;
                icon(hwnd, inner, false);
            }
            Apply::Menu(entries) => menu(inner, &entries),
        }
    }
}

pub fn install(app: &AppHandle, conf: Option<&str>) -> io::Result<()> {
    // Explorer follows SystemUsesLightTheme, independently of the app theme.
    use windows_sys::Win32::System::Registry::*;
    let mut light = 0u32;
    let mut size = std::mem::size_of_val(&light) as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize").as_ptr(),
            wide("SystemUsesLightTheme").as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut light as *mut u32).cast(),
            &mut size,
        );
    }
    let configured = conf
        .and_then(|text| zerus_client_core::menubar::tray_conf_value(text, "icon-color"))
        .and_then(|text| zerus_client_core::trayicon::parse_color(&text));
    let foreground = zerus_client_core::trayicon::foreground(light != 1, configured);
    let image = drawing::tile(
        &Face::counts(0, 0, false),
        unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16),
        foreground,
    )?;
    let class = wide("ZerusNextNotificationArea");
    let native = unsafe { GetModuleHandleW(std::ptr::null()) };
    let descriptor = WNDCLASSW {
        lpfnWndProc: Some(procedure),
        hInstance: native,
        lpszClassName: class.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    if unsafe { RegisterClassW(&descriptor) } == 0
        && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
    {
        return Err(io::Error::last_os_error());
    }
    let inner = Inner {
        bar: Bar::default(),
        menu: std::ptr::null_mut(),
        bindings: HashMap::new(),
        icon: image,
        foreground,
        tooltip: "Zerus".into(),
        badge: None,
        current_notice: Default::default(),
    };
    let raw = Box::into_raw(Box::new(State {
        app: app.clone(),
        taskbar_created: unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) },
        inner: Mutex::new(inner),
    }));
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
            raw.cast(),
        )
    };
    if hwnd.is_null() {
        drop(unsafe { Box::from_raw(raw) });
        return Err(io::Error::last_os_error());
    }
    let added = icon(
        hwnd,
        &state(hwnd)
            .unwrap()
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
        true,
    );
    if !added {
        unsafe {
            DestroyWindow(hwnd);
            drop(Box::from_raw(raw));
        }
        return Err(io::Error::last_os_error());
    }
    app.manage(WindowsShell {
        hwnd: AtomicUsize::new(hwnd as usize),
        state: AtomicUsize::new(raw as usize),
    });
    Ok(())
}
fn on_main(app: &AppHandle, work: impl FnOnce(HWND, &State) + Send + 'static) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(shell) = handle.try_state::<WindowsShell>() else {
            return;
        };
        let hwnd = shell.hwnd.load(Ordering::Acquire) as HWND;
        if let Some(state) = state(hwnd) {
            work(hwnd, state);
        }
    });
}
pub fn offer(
    app: &AppHandle,
    face: Face,
    tooltip: String,
    entries: Vec<Entry>,
    badge: Option<String>,
) {
    on_main(app, move |hwnd, state| {
        {
            let mut inner = state.inner.lock().unwrap_or_else(PoisonError::into_inner);
            let offered = inner.bar.offer(face, tooltip, entries, badge.clone());
            inner.badge = badge;
            apply(hwnd, &mut inner, offered.changes);
        }
        apply_badge(&state.app);
    });
}
pub fn apply_badge(app: &AppHandle) {
    let Some(shell) = app.try_state::<WindowsShell>() else {
        return;
    };
    let hwnd = shell.hwnd.load(Ordering::Acquire) as HWND;
    let Some(state) = state(hwnd) else { return };
    let badge = state
        .inner
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .badge
        .clone();
    let Some(window) = crate::main_window(app) else {
        return;
    };
    let icon = badge.and_then(|text| drawing::badge(&text).ok());
    let image = icon
        .as_ref()
        .map(|icon| tauri::image::Image::new_owned(icon.rgba.clone(), icon.size, icon.size));
    if let Err(e) = window.set_overlay_icon(image) {
        eprintln!("zerus-client: taskbar badge: {e}");
    }
}
pub fn deliver(app: &AppHandle, cleared: Vec<String>, raised: Vec<Notice>) {
    on_main(app, move |hwnd, state| {
        let mut inner = state.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.current_notice.cleared(&cleared);
        for notice in raised {
            let mut data = data(hwnd);
            data.uFlags = NIF_INFO;
            data.szInfoTitle = crate::windows_shell::wide_text(&notice.title);
            data.szInfo = crate::windows_shell::wide_text(&notice.body);
            data.Anonymous.uTimeout = 15000;
            data.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON;
            // The app icon, not Windows' generic information symbol (Qt parity).
            data.hBalloonIcon = unsafe {
                LoadIconW(
                    GetModuleHandleW(std::ptr::null()),
                    std::ptr::without_provenance::<u16>(
                        tauri::utils::platform::WINDOWS_APP_ICON_RESOURCE_ID as usize,
                    ),
                )
            };
            if data.hBalloonIcon.is_null() {
                data.hBalloonIcon = inner.icon.handle;
            }
            if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) } != 0 {
                inner.current_notice.raised(notice.token);
            } else if let Some(sender) = crate::desk::sender(&state.app) {
                let _ = sender.send(DeskInput::Failed {
                    token: notice.token,
                    detail: "This desktop does not show tray notifications.".into(),
                });
            }
        }
    });
}
pub fn stop(app: &AppHandle) {
    if let Some(worker) = app.try_state::<super::worker::Worker>() {
        worker.stop();
    }
    if let Some(window) = crate::main_window(app) {
        let _ = window.set_overlay_icon(None);
    }
    if let Some(shell) = app.try_state::<WindowsShell>() {
        let hwnd = shell.hwnd.swap(0, Ordering::AcqRel) as HWND;
        if !hwnd.is_null() {
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &data(hwnd));
                DestroyWindow(hwnd);
            }
        }
        let raw = shell.state.swap(0, Ordering::AcqRel) as *mut State;
        if !raw.is_null() {
            drop(unsafe { Box::from_raw(raw) });
        }
    }
}
pub fn open_running(
    app: &AppHandle,
    computer: &zerus_client_core::ComputerId,
    name: &str,
    tty: &str,
) {
    use zerus_client_core::terminal_launch::{Launcher, Opened};
    let settings = crate::desk::terminal_settings(&crate::data_dir(app));
    let mode = crate::desk::current_mode(app);
    let hgs = zerus_client_core::default_hgs();
    let command = zerus_client_core::shell::open_command(
        &hgs.to_string_lossy(),
        &computer.0,
        name,
        None,
        zerus_client_core::shell::current_shell(),
    );
    if mode == OpenMode::Clipboard {
        match crate::clipboard::copy_text(app.clone(), command.clone()) {
            Ok(()) => crate::desk::window_notice(
                app,
                "Zerus: copied to the clipboard",
                &zerus_client_core::shell::notification_text(&command),
            ),
            Err(e) => crate::desk::window_notice(app, "Zerus: the terminal did not open", &e),
        }
        return;
    }
    let mut launcher = Launcher::new(settings);
    match launcher.open_session(&hgs.to_string_lossy(), &computer.0, name, tty) {
        Ok(Opened::Opened {
            advisory: Some(text),
        }) => crate::desk::window_notice(
            app,
            zerus_client_core::terminal_launch::ADVISORY_TITLE,
            &text,
        ),
        Err(e) => crate::desk::window_notice(
            app,
            zerus_client_core::terminal_launch::NOT_OPENED_TITLE,
            &zerus_client_core::terminal_launch::not_opened(&e, &command),
        ),
        _ => {}
    }
}
