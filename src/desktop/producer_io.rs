//! Bounded control I/O only. No production caller, controller or authority.
use super::transport::Stream;
#[cfg(unix)]
use anyhow::ensure;
use anyhow::Result;
#[cfg(unix)]
use std::io::{BufRead, Read, Write};
use std::{
    io::{self, BufReader},
    time::{Duration, Instant},
};

pub(super) const LINE_BYTES: usize = 16 << 10;
pub(super) const MAX_WAIT: Duration = zerus_client_core::desktop::producer::timing::IDLE_LIMIT;

/// Own the supplied reader and its buffered tail, with no FD clone or reset.
/// Same-user transport evidence only, never canonical instance or role proof.
pub(super) struct Channel {
    reader: BufReader<Stream>,
    closed: bool,
}
impl Channel {
    pub fn new(reader: BufReader<Stream>) -> Result<Self> {
        #[cfg(unix)]
        {
            let channel = Self {
                reader,
                closed: false,
            };
            super::transport::authenticate(channel.reader.get_ref())?;
            Ok(channel)
        }
        #[cfg(not(unix))]
        {
            let _ = reader;
            anyhow::bail!("producer control I/O is unsupported")
        }
    }

    pub fn line(&mut self, until: Instant) -> Result<Option<Vec<u8>>> {
        anyhow::ensure!(!self.closed, "producer control connection is closed");
        let result = line(&mut self.reader, until);
        if !matches!(&result, Ok(Some(_))) {
            self.close();
        }
        result
    }

    pub fn write_line(&mut self, bytes: &[u8], until: Instant) -> Result<()> {
        anyhow::ensure!(!self.closed, "producer control connection is closed");
        let result = write_line(self.reader.get_mut(), bytes, until);
        if result.is_err() {
            self.close();
        }
        result
    }

    fn close(&mut self) {
        if !self.closed {
            // Fence access first, including when shutdown itself cannot succeed.
            self.closed = true;
            super::transport::shutdown(self.reader.get_ref());
        }
    }
}
impl Drop for Channel {
    fn drop(&mut self) {
        self.close();
    }
}

fn remaining(until: Instant) -> io::Result<Duration> {
    let left = until.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    if left > MAX_WAIT {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(left)
}

#[cfg(unix)]
struct Until<'a> {
    reader: &'a mut BufReader<Stream>,
    until: Instant,
}
#[cfg(unix)]
impl Until<'_> {
    fn prepare(&self) -> io::Result<()> {
        let timeout = remaining(self.until)?;
        receive_timeout_result(
            self.reader.get_ref().set_read_timeout(Some(timeout)),
            cfg!(target_vendor = "apple"),
        )
    }
}

#[cfg(unix)]
fn receive_timeout_result(result: io::Result<()>, apple: bool) -> io::Result<()> {
    // The supplied timeout is already valid, nonzero and <=15s. XNU's
    // sosetoptlock rejects options with EINVAL after both socket directions
    // shut down. Reading can then drain queued bytes/EOF without blocking.
    // Keep the read itself and both absolute deadline checks authoritative.
    if apple
        && result
            .as_ref()
            .is_err_and(|e| e.raw_os_error() == Some(libc::EINVAL))
    {
        Ok(())
    } else {
        result
    }
}
#[cfg(unix)]
impl Read for Until<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.prepare()?;
        self.reader.read(bytes)
    }
}
#[cfg(unix)]
impl BufRead for Until<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.prepare()?;
        self.reader.fill_buf()
    }
    fn consume(&mut self, count: usize) {
        self.reader.consume(count);
    }
}

/// Off-actor only. Reuse the existing reader, including its buffered tail.
/// One total deadline, at most 15 seconds, includes all partial progress. The
/// returned line includes LF; content/schema/identity still need validation.
pub(super) fn line(reader: &mut BufReader<Stream>, until: Instant) -> Result<Option<Vec<u8>>> {
    #[cfg(unix)]
    {
        remaining(until)?;
        let bytes = super::protocol::line(&mut Until { reader, until }, LINE_BYTES)?;
        remaining(until)?;
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = (reader, until);
        anyhow::bail!("producer control I/O is unsupported")
    }
}

fn frame_valid(bytes: &[u8]) -> bool {
    (2..=LINE_BYTES).contains(&bytes.len())
        && bytes.last() == Some(&b'\n')
        && !bytes[..bytes.len() - 1].contains(&b'\n')
}

/// Writes one already encoded line, without cloning/closing the owned stream.
/// Ok means local write completion only, never peer acceptance or an effect
/// receipt. Any failed/late write may have sent bytes; it never permits replay.
pub(super) fn write_line(stream: &mut Stream, bytes: &[u8], until: Instant) -> Result<()> {
    #[cfg(unix)]
    {
        ensure!(frame_valid(bytes), "invalid producer control line");
        remaining(until)?;
        let mut written = 0;
        while written < bytes.len() {
            stream.set_write_timeout(Some(remaining(until)?))?;
            match stream.write(&bytes[written..]) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                Ok(count) => written += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
                Err(error) => return Err(error.into()),
            }
        }
        remaining(until)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (stream, bytes, until);
        anyhow::bail!("producer control I/O is unsupported")
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
mod channel_tests;
