//! Native Windows shell. No webview capability exposes the OS adapters.
use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::{
    Foundation::*, Security::Authorization::*, Security::*, System::Threading::*,
};

#[cfg(feature = "live")]
mod drawing;
pub mod foreground;
// WhiteWh fork: hgs-tray.exe --light, the icon without the desktop.
#[cfg(feature = "live")]
pub mod light;
#[cfg(feature = "live")]
pub mod startup;
#[cfg(feature = "live")]
pub mod tray;
#[cfg(feature = "live")]
pub mod worker;

thread_local! {
    /// The watchdog's beat, run by the main thread's timer.
    static BEAT: std::cell::RefCell<Option<Box<dyn Fn()>>> = const { std::cell::RefCell::new(None) };
}

/// The main thread's heartbeat for the watchdog (zerus-5wmh.13.14.3): a thread
/// timer every `every`. Its WM_TIMER is dispatched by the main message loop
/// and by every modal loop on the thread (menus, dialogs, file pickers), as
/// Qt's main-thread QTimer was, so only a main thread that stops dispatching
/// misses beats. Call on the main thread.
pub fn heartbeat(every: std::time::Duration, beat: impl Fn() + 'static) -> io::Result<()> {
    unsafe extern "system" fn tick(_: HWND, _: u32, _: usize, _: u32) {
        BEAT.with(|beat| {
            if let Some(beat) = beat.borrow().as_ref() {
                beat();
            }
        });
    }
    BEAT.with(|slot| *slot.borrow_mut() = Some(Box::new(beat)));
    let every = u32::try_from(every.as_millis()).unwrap_or(u32::MAX).max(1);
    // SAFETY: a thread timer (no window) with a static callback; it lives as
    // long as the main thread.
    let timer = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::SetTimer(
            std::ptr::null_mut(),
            0,
            every,
            Some(tick),
        )
    };
    if timer == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
pub(crate) fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
}

pub(crate) fn token_sid(token: &OwnedHandle) -> io::Result<String> {
    let mut needed = 0;
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
    }
    if needed == 0 || needed > 64 * 1024 {
        return Err(io::Error::other("Cannot read the Windows user SID"));
    }
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
    let mut text = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut length = 0;
    while length < 256 && unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    let result = if length < 256 {
        String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
            .map_err(io::Error::other)
    } else {
        Err(io::Error::other("Invalid Windows user SID"))
    };
    unsafe {
        LocalFree(text.cast());
    }
    result
}
pub(crate) fn own_sid() -> io::Result<String> {
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    token_sid(&owned(token)?)
}
pub(crate) struct Security(*mut c_void);
impl Security {
    pub fn user_only() -> io::Result<Self> {
        let sddl = wide(&format!("D:P(A;;GA;;;{})", own_sid()?));
        let mut descriptor = std::ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(descriptor))
        }
    }
    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}
impl Drop for Security {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
