mod accounts;
mod agent_guide;
mod analytics;
mod approval_log;
mod cli;
mod config;
mod desktop;
mod download;
mod host;
mod machines;
mod macos_session;
mod metrics;
#[cfg(unix)]
mod mobile_peers;
mod native_ui;
mod path_info;
mod phones;
mod platform;
mod presence;
mod remote_shell;
mod project_launcher;
mod screen;
mod self_exec;
mod session_reference;
mod speech;
mod ssh_bytes;
mod sqlite_file;
mod state;
mod status;
mod swarm;
mod tasks;
#[cfg(all(test, unix))]
mod test_process;
mod watch;

fn main() {
    // A native context callback is an optional bounded read, never ordinary
    // CLI dispatch (which can prepare managers, peers and live-session work).
    if std::env::args().nth(1).as_deref() == Some("--opencode-agent-hint") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        println!("{}", serde_json::json!({"context":state::opencode::hint_context(&args)}));
        return;
    }
    #[cfg(windows)]
    let entered = std::time::Instant::now();
    zerus_client_core::quit_origin::capture_process_entry();
    // A creation launch child keeps the desktop actor's custody lock for its
    // own lifetime only: marked close-on-exec before anything else starts;
    // after the pure raw-input capture, which reads the unmodified
    // environment and starts nothing.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Err(error) = desktop::creation_launch::adopt_custody() {
        eprintln!("hgs: {error}");
        std::process::exit(2);
    }
    // The same for a task message child, with its own descriptor.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Err(error) = desktop::task_message::adopt_custody() {
        eprintln!("hgs: {error}");
        std::process::exit(2);
    }
    // The same for a deferred send child, with its own descriptor.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Err(error) = desktop::deferred_send::adopt_custody() {
        eprintln!("hgs: {error}");
        std::process::exit(2);
    }
    // The same for a deferred answer child, with its own descriptor.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Err(error) = desktop::deferred_answer::adopt_custody() {
        eprintln!("hgs: {error}");
        std::process::exit(2);
    }
    // The same for a compact-to-send child, with its own descriptor.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Err(error) = desktop::compact_to_send::adopt_custody() {
        eprintln!("hgs: {error}");
        std::process::exit(2);
    }
    // The same for a session resume child, with its own descriptor.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Err(error) = desktop::session_resume::adopt_custody() {
        eprintln!("hgs: {error}");
        std::process::exit(2);
    }
    // Byte framing is local SSH plumbing, before configuration or managers.
    if std::env::args().nth(1).as_deref() == Some("--ssh-bytes-client") {
        let code = match ssh_bytes::run(std::env::args().skip(2).collect()) {
            Ok(code) => code,
            Err(error) => { eprintln!("hgs: Windows SSH pipe: {error}"); 255 }
        };
        std::process::exit(code);
    }
    // COM cold activation cannot load config, create jobs or acquire ownership.
    #[cfg(windows)]
    if let Some(code) = desktop::windows_notification_activation(std::env::args().nth(1).as_deref())
    {
        std::process::exit(code);
    }
    // Native implementation workers validate a fixed bounded frame before
    // selecting configuration or any ownership/manager operation.
    #[cfg(windows)]
    if let Some(code) =
        desktop::windows_registration_entry(&std::env::args().skip(1).collect::<Vec<_>>())
    {
        std::process::exit(code);
    }
    if std::env::args().nth(1).as_deref() == Some("--macos-session-service") {
        std::process::exit(macos_session::serve());
    }
    // The Windows session host and its tools, before `cli::dispatch` puts
    // hgs.exe into its own kill-on-close job (spec D1, D4).
    #[cfg(windows)]
    if std::env::args().nth(1).as_deref() == Some("__host") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        std::process::exit(host::run(&args));
    }
    #[cfg(windows)]
    let result = cli::dispatch_at(std::env::args().skip(1).collect(), entered);
    #[cfg(not(windows))]
    let result = cli::dispatch(std::env::args().skip(1).collect());
    let code = match result {
        Ok(code) => code,
        Err(err) => {
            #[cfg(windows)]
            desktop::startup_diagnostic::record(
                &std::env::args().skip(1).collect::<Vec<_>>(),
                err.code,
                &err.message,
            );
            // One write (eprintln! writes each piece on its own), so that no
            // frame a pseudoconsole paints shows the prefix without the message.
            let line = format!("hgs: {}\n", err.message);
            eprint!("{line}");
            err.code
        }
    };
    std::process::exit(code);
}

#[cfg(all(test, target_os = "linux"))]
#[path = "../tests/support/executable.rs"]
mod test_executable;
