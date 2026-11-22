// No console window next to the app in Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(any(feature = "live", target_os = "macos", windows))]
    zerus_client_core::quit_origin::capture_process_entry();
    // The service's fixed inherited socket/owner entry precedes renderer,
    // Tauri context, instance claims and every ordinary GUI/writer setup.
    #[cfg(target_os = "macos")]
    if let Some(status) = zerus_client_lib::entry::notification_host(
        std::env::args_os().skip(1),
        zerus_client_lib::notification_host::run,
    ) {
        std::process::exit(status);
    }
    // A launch's console window on Windows: this program started with
    // `--console-run` runs the program in it and ends with its exit code.
    #[cfg(all(windows, feature = "live"))]
    {
        use zerus_client_core::terminal_launch::console_host;
        let arguments: Vec<String> = std::env::args_os()
            .skip(1)
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        if arguments.first().map(String::as_str) == Some(console_host::OPTION) {
            std::process::exit(console_host::run(&arguments));
        }
        // WhiteWh fork: light mode, the icon and the session host keeper only;
        // before any Tauri, webview or instance setup.
        if arguments.first().map(String::as_str) == Some(zerus_client_lib::LIGHT_FLAG) {
            std::process::exit(zerus_client_lib::light_mode());
        }
    }
    zerus_client_lib::run();
}
