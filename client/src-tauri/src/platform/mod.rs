//! Platform integrations of the client.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(all(target_os = "linux", feature = "live"))]
pub mod linux_notify;
#[cfg(all(test, target_os = "linux"))]
pub(crate) mod linux_test_bus;
#[cfg(all(target_os = "linux", feature = "live"))]
pub mod linux_tray;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub(crate) mod macos_notification_request;
#[cfg(target_os = "macos")]
pub mod macos_power;
// The menu bar drives the desk thread, which exists only with the live data source.
#[cfg(all(target_os = "macos", feature = "live"))]
pub mod macos_menubar;
// Attention notifications come from the desk thread too.
#[cfg(all(target_os = "macos", feature = "live"))]
pub mod macos_notify;

#[cfg(all(target_os = "linux", feature = "live"))]
pub mod linux_window_layer;

#[cfg(windows)]
pub mod windows;
