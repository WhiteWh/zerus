// No console window next to the app in Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
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
    }
    zerus_client_lib::run();
}
