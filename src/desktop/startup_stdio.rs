//! One fixed-deadline local helper. It holds negative exclusion only, never a
//! profile writer or a producer session. No desktop state is created for absence.
#[cfg(unix)]
use super::startup_exclusion::Startup;
use super::store::Location;
use anyhow::Result;
#[cfg(unix)]
use anyhow::{ensure, Context};
#[cfg(unix)]
use zerus_client_core::desktop::startup::{Frame, FRAME_BYTES, WAIT};

pub(super) fn run(location: &Location) -> Result<()> {
    #[cfg(unix)]
    {
        exchange(location, std::time::Instant::now() + WAIT)
    }
    #[cfg(not(unix))]
    {
        let _ = location;
        anyhow::bail!("desktop startup exclusion is unsupported")
    }
}

#[cfg(unix)]
fn line(deadline: std::time::Instant) -> Result<Vec<u8>> {
    use std::{io, time::Instant};
    let mut bytes = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        ensure!(!left.is_zero(), "desktop startup input deadline");
        let mut fd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
        // Read only one byte after readiness. No buffered read can hide another
        // frame or turn partial progress into a renewed deadline.
        let ready = unsafe { libc::poll(&mut fd, 1, ms) };
        if ready < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        ensure!(
            ready > 0 && Instant::now() < deadline,
            "desktop startup input unavailable"
        );
        let mut byte = [0];
        let read = unsafe { libc::read(libc::STDIN_FILENO, byte.as_mut_ptr().cast(), 1) };
        if read < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        ensure!(read == 1, "desktop startup input ended");
        bytes.push(byte[0]);
        ensure!(
            bytes.len() <= FRAME_BYTES,
            "desktop startup frame too large"
        );
        if byte[0] == b'\n' {
            return Ok(bytes);
        }
    }
}
#[cfg(unix)]
fn exchange(location: &Location, deadline: std::time::Instant) -> Result<()> {
    use std::{
        io::{self, Write},
        time::Instant,
    };
    let request: Frame =
        serde_json::from_slice(&line(deadline)?).context("invalid startup frame")?;
    request
        .checked("hold", request.nonce())
        .context("invalid startup hold")?;
    let held = Startup::hold(location)?;
    ensure!(Instant::now() < deadline, "desktop startup deadline");
    let emit = |op| -> Result<()> {
        // These two closed fixed-width metadata replies together fit below the
        // POSIX minimum PIPE_BUF; no app body or unbounded output is written.
        let bytes = Frame::encode(op, request.nonce(), Some(held.configuration()))
            .context("invalid startup reply")?;
        ensure!(
            bytes.len() < 256 && Instant::now() < deadline,
            "desktop startup output deadline"
        );
        let mut out = io::stdout().lock();
        out.write_all(&bytes)?;
        out.flush()?;
        Ok(())
    };
    emit("ready")?;
    let release: Frame =
        serde_json::from_slice(&line(deadline)?).context("invalid startup release")?;
    release
        .checked("release", request.nonce())
        .context("invalid startup release")?;
    ensure!(
        release.configuration() == Some(held.configuration()),
        "startup configuration changed"
    );
    emit("released")?;
    // The parent also requires EOF and actual helper exit before publishing its
    // configuration. The reply alone is never evidence that this lock dropped.
    drop(held);
    Ok(())
}
