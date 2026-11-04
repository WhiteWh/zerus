//! Only probe/watch helpers, each spawned into its own group. Never used for
//! agent execution, process groups supplied by callers, or detached sessions.
use std::{
    io,
    process::{Child, Command, ExitStatus},
};

pub(super) fn prepare(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = command;
}

pub(super) fn try_wait(child: &mut Child) -> io::Result<Option<ExitStatus>> {
    #[cfg(unix)]
    {
        if exited_unreaped(child)? {
            // The leader's still-reserved PID prevents PGID reuse, including
            // when it exits before its pipe-holding ssh/ProxyCommand child.
            signal_and_reap(child, true).map(Some)
        } else {
            Ok(None)
        }
    }
    #[cfg(not(unix))]
    child.try_wait()
}

pub(super) fn terminate(child: &mut Child) -> io::Result<ExitStatus> {
    #[cfg(unix)]
    {
        // Also establishes exclusive wait ownership when the child is live.
        // ECHILD/unsupported inspection never signals an unverified PID/group.
        let exited = exited_unreaped(child)?;
        signal_and_reap(child, exited)
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
        child.wait()
    }
}

#[cfg(unix)]
fn exited_unreaped(child: &Child) -> io::Result<bool> {
    // Only this supervisor observes/reaps this helper; do not call Child's
    // try_wait first. WNOWAIT retains a terminated leader until group cleanup.
    // https://man7.org/linux/man-pages/man2/waitid.2.html
    let mut policy: libc::sigaction = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut policy) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if policy.sa_sigaction == libc::SIG_IGN || policy.sa_flags & libc::SA_NOCLDWAIT != 0 {
        return Err(io::Error::other("helper wait ownership is unavailable"));
    }
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    for _ in 0..3 {
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            let pid = unsafe { info.si_pid() };
            if pid != 0 && pid as u32 != child.id() {
                return Err(io::Error::other("helper wait identity changed"));
            }
            return Ok(pid != 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Err(io::ErrorKind::Interrupted.into())
}

#[cfg(unix)]
fn signal_and_reap(child: &mut Child, exited: bool) -> io::Result<ExitStatus> {
    #[cfg(not(target_os = "macos"))]
    let _ = exited;
    let pid =
        libc::pid_t::try_from(child.id()).map_err(|_| io::Error::other("invalid helper PID"))?;
    // prepare() made precisely this child the group leader. An unreaped child
    // pins that identity until wait(). No process scan, name/path match or
    // shared group is an authority to signal. setsid-detached children escape
    // this group intentionally and their open pipes remain accounted.
    if unsafe { libc::killpg(pid, libc::SIGKILL) } != 0 {
        let error = io::Error::last_os_error();
        // XNU excludes zombies from signal recipients and reports EPERM when
        // the group contains only our unreaped leader. Do not ignore arbitrary
        // EPERM: a fixed two-PID kernel query must prove that exact sole member.
        // https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_sig.c
        #[cfg(target_os = "macos")]
        let zombie_only = exited && error.raw_os_error() == Some(libc::EPERM) && sole_member(pid);
        #[cfg(not(target_os = "macos"))]
        let zombie_only = false;
        if error.raw_os_error() != Some(libc::ESRCH) && !zombie_only {
            return Err(error);
        }
    }
    child.wait()
}

#[cfg(target_os = "macos")]
fn sole_member(pid: libc::pid_t) -> bool {
    let mut members = [0 as libc::pid_t; 2];
    let found = unsafe {
        libc::proc_listpgrppids(
            pid,
            members.as_mut_ptr().cast(),
            std::mem::size_of_val(&members) as libc::c_int,
        )
    };
    found == 1 && members[0] == pid
}

#[cfg(all(test, unix))]
mod tests;
