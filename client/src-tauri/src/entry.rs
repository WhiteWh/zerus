//! Dispatch the fixed headless role before renderer or GUI admission. The
//! callback still authenticates the inherited service socket and owner FD;
//! selecting a role alone grants no native notification authority.
use std::ffi::OsStr;

pub fn notification_host(
    arguments: impl IntoIterator<Item = impl AsRef<OsStr>>,
    authenticated_host: impl FnOnce() -> i32,
) -> Option<i32> {
    if arguments
        .into_iter()
        .any(|argument| argument.as_ref() == "--desktop-notification-host")
    {
        Some(authenticated_host())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn background_service_gui_is_not_the_authenticated_headless_role() {
        assert_eq!(
            super::notification_host(["--background"], || panic!("GUI")),
            None
        );
        assert_eq!(
            super::notification_host(["--desktop-notification-host"], || 17),
            Some(17)
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            zerus_client_core::desktop::notifications::ENTRY,
            "--desktop-notification-host"
        );
    }
}
