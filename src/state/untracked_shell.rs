//! Close one confirmation-bound untracked tmux shell. No archive or cleanup.
use super::*;
use zerus_client_core::untracked_shell::{Identity, UNVERIFIED};
#[cfg(unix)]
use {crate::platform, std::fs};

pub(super) fn capture(name: &str) -> Result<Identity> {
    let _guard = lock(None)?;
    capture_locked(name)
}

#[cfg(not(unix))]
fn capture_locked(_: &str) -> Result<Identity> {
    Err(UNVERIFIED.into())
}

#[cfg(unix)]
fn capture_locked(name: &str) -> Result<Identity> {
    use backend::PaneField::*;
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    if record_path(name).try_exists().map_err(|_| UNVERIFIED)?
        || dsh::exists(name)
        || opencode::exists(name)
    {
        return Err(UNVERIFIED.into());
    }
    let info = backend::current()
        .pane_info(
            backend::Target::Name(name),
            &[
                SessionId,
                SessionName,
                SocketPath,
                SessionCreated,
                SessionWindows,
                SessionGrouped,
                WindowPanes,
                WindowLinked,
                PaneId,
                PanePid,
                ServerPid,
                CurrentCommand,
                Dead,
                RunMarker,
            ],
        )
        .map_err(|_| UNVERIFIED)?;
    let fields = info.fields();
    let [session, actual_name, socket, created, windows, grouped, panes, linked, pane, pid, server, command, dead, marker] =
        fields.as_slice()
    else {
        return Err(UNVERIFIED.into());
    };
    if *actual_name != name
        || *windows != "1"
        || *panes != "1"
        || *grouped != "0"
        || *linked != "0"
        || *dead != "0"
        || !marker.is_empty()
    {
        return Err(UNVERIFIED.into());
    }
    let metadata = fs::symlink_metadata(socket).map_err(|_| UNVERIFIED)?;
    if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(UNVERIFIED.into());
    }
    let pid = pid.parse().map_err(|_| UNVERIFIED)?;
    let server_pid = server.parse().map_err(|_| UNVERIFIED)?;
    let identity = Identity {
        version: 1,
        socket: socket.to_string(),
        socket_dev: metadata.dev().to_string(),
        socket_ino: metadata.ino().to_string(),
        server_pid,
        server_start: platform::process_identity(server_pid)
            .ok_or(UNVERIFIED)?
            .birth,
        session_id: session.to_string(),
        session_created: created.parse().map_err(|_| UNVERIFIED)?,
        pane_id: pane.to_string(),
        pane_pid: pid,
        pane_start: platform::process_identity(pid).ok_or(UNVERIFIED)?.birth,
        shell: command.to_string(),
    };
    if !identity.valid() {
        return Err(UNVERIFIED.into());
    }
    let after = fs::symlink_metadata(socket).map_err(|_| UNVERIFIED)?;
    if metadata.dev() != after.dev()
        || metadata.ino() != after.ino()
        || platform::process_identity(pid).is_none_or(|p| p.birth != identity.pane_start)
        || platform::process_identity(server_pid).is_none_or(|p| p.birth != identity.server_start)
    {
        return Err(UNVERIFIED.into());
    }
    Ok(identity)
}

pub(super) fn terminate(
    name: &str,
    expected: &Identity,
    scope: &session_action::Scope,
) -> Result<()> {
    if !expected.valid() {
        return Err(UNVERIFIED.into());
    }
    #[cfg(not(unix))]
    {
        let _ = (name, scope);
        Err(UNVERIFIED.into())
    }
    #[cfg(unix)]
    {
        let pane = {
            let _guard = lock(None)?;
            if capture_locked(name)? != *expected {
                return Err(UNVERIFIED.into());
            }
            // The acknowledgement owner must survive its own terminal action.
            let mut caller = std::process::id();
            let mut outside = false;
            for _ in 0..128 {
                if caller == expected.pane_pid {
                    return Err(
                        "Run Terminate from outside this shell. Its terminal was kept.".into(),
                    );
                }
                if caller <= 1 {
                    outside = true;
                    break;
                }
                caller = platform::parent_and_name(caller)
                    .map_err(|_| UNVERIFIED)?
                    .0
                    .ok_or(UNVERIFIED)?;
            }
            if !outside {
                return Err(UNVERIFIED.into());
            }
            let pane = platform::PinnedProcess::pin(expected.pane_pid, &expected.pane_start)
                .map_err(|_| UNVERIFIED)?;
            let server = platform::PinnedProcess::pin(expected.server_pid, &expected.server_start)
                .map_err(|_| UNVERIFIED)?;
            if pane.state().map_err(|_| UNVERIFIED)? == platform::ProcessState::Exited
                || server.state().map_err(|_| UNVERIFIED)? == platform::ProcessState::Exited
            {
                return Err(UNVERIFIED.into());
            }
            // Evaluate the terminal facts again in the server's command queue.
            // Only the original immutable session ID is an effect target.
            let checks = [
                ("session_id", expected.session_id.clone()),
                ("session_created", expected.session_created.to_string()),
                ("pane_id", expected.pane_id.clone()),
                ("pane_pid", expected.pane_pid.to_string()),
                ("pid", expected.server_pid.to_string()),
                ("pane_current_command", expected.shell.clone()),
                ("session_windows", "1".into()),
                ("window_panes", "1".into()),
                ("session_grouped", "0".into()),
                ("window_linked", "0".into()),
                ("pane_dead", "0".into()),
                ("@hgs_run", String::new()),
            ];
            let condition = checks
                .into_iter()
                .rev()
                .fold("1".to_owned(), |rest, (key, value)| {
                    format!("#{{&&:#{{==:#{{{key}}},{value}}},{rest}}}")
                });
            scope.changing();
            let output = platform::output_until(std::process::Command::new("tmux")
                .args(["-S", &expected.socket, "if-shell", "-F", "-t", &expected.session_id,
                    &condition, &format!("kill-session -t {}", expected.session_id),
                    "display-message -p HGS_UNTRACKED_TERMINAL_CHANGED"]),
                std::time::Duration::from_secs(3))
                .map_err(|_| "Shell termination could not be confirmed. Check its terminal before retrying; the working folder was kept.".to_string())?;
            if output.status.success() && output.stdout == b"HGS_UNTRACKED_TERMINAL_CHANGED\n" {
                scope.unchanged();
                return Err(UNVERIFIED.into());
            }
            if !output.status.success() || !output.stdout.is_empty() {
                return Err("Shell termination could not be confirmed. Check its terminal before retrying; the working folder was kept.".into());
            }
            pane
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if pane
                .state()
                .is_ok_and(|s| s == platform::ProcessState::Exited)
            {
                break;
            }
            if std::time::Instant::now() >= deadline {
                return Err("The shell terminal closed, but its process exit could not be confirmed. The working folder was kept.".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        scope.result(name, "", "", None);
        Ok(())
    }
}
