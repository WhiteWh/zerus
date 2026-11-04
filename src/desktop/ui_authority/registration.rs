use super::*;

/// Explicit installer mode changes use the same agent refusal as native
/// policy requests. This proves a caller boundary, never a service grant.
pub(in crate::desktop) fn caller(scope: &Scope) -> Result<&'static str> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "use the desktop user, without sudo"
    );
    ensure!(
        ["HGS_SESSION", "HGS_RUN_ID"]
            .iter()
            .all(|name| std::env::var_os(name).is_none()),
        "agent session cannot change desktop mode; use Settings or a terminal outside Zerus"
    );
    let until = Instant::now() + WAIT;
    #[cfg(target_os = "macos")]
    let owned = macos_ancestry::inspect(std::process::id(), until)?;
    #[cfg(not(target_os = "macos"))]
    check_ancestry(std::process::id(), until)?;
    if let Ok(tty) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open("/dev/tty")
    {
        // SAFETY: the owned descriptor stays open through both queries.
        if unsafe { libc::isatty(tty.as_raw_fd()) } == 1
            && unsafe { libc::tcgetpgrp(tty.as_raw_fd()) } == unsafe { libc::getpgrp() }
        {
            return Ok("interactive_terminal");
        }
    }
    #[cfg(target_os = "macos")]
    for process in owned.iter().skip(1) {
        ensure!(
            Instant::now() < until && process.current(),
            "installer caller changed"
        );
        if scope.selected_frontend(Path::new(&process.executable)) {
            ensure!(process.current(), "installer frontend changed");
            return Ok("installed_frontend");
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let original = Process::read(std::process::id())?;
        ensure!(original.current(), "installer caller changed");
        let mut pid = original.identity.parent;
        let mut seen = BTreeSet::new();
        while pid != 0 {
            ensure!(
                seen.len() < ANCESTORS && Instant::now() < until && seen.insert(pid),
                "installer caller ancestry unavailable"
            );
            if !same_user(pid)? {
                break;
            }
            let process = Process::read(pid)?;
            ensure!(process.current(), "installer caller changed");
            if scope.selected_frontend(Path::new(&process.executable)) {
                return Ok("installed_frontend");
            }
            pid = process.identity.parent;
        }
    }
    anyhow::bail!("desktop mode changes require Settings or a foreground terminal outside Zerus")
}
