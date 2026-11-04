use super::*;

// zbus 5.19.0 src/connection/socket/unix.rs:277-291 converts successful
// recvmsg(bytes=0) into this exact synthetic error, before returning any FD
// vector. Its rustix ancillary RAII guard drains/drops on that return. The
// concrete Async<UnixStream> half comes only from our private connector.
// async-io 2.6 / polling's Linux readiness path never synthesizes this literal.
// A dependency change must pass the genuine clean-startup/CleanExit test in CI.
pub(super) fn sdk_empty_eof(error: &io::Error) -> bool {
    if error.kind() != io::ErrorKind::BrokenPipe || error.raw_os_error().is_some() {
        return false;
    }
    // Exact Display comparison without to_string/format or owned text. A long
    // or different fragment refuses immediately, never accumulates a buffer.
    struct Literal(&'static str);
    impl std::fmt::Write for Literal {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            self.0 = self.0.strip_prefix(text).ok_or(std::fmt::Error)?;
            Ok(())
        }
    }
    use std::fmt::Write;
    let mut literal = Literal("failed to read from socket");
    write!(&mut literal, "{error}").is_ok() && literal.0.is_empty()
}

pub(super) struct Frame {
    pub(super) inner: Option<Box<dyn ReadHalf>>,
    gate: Arc<Ingress>,
    primary: [u8; 16],
    filled: usize,
    admitted: bool,
    pub(super) returned: bool,
    pub(super) empty_close_eof: bool,
}
impl Frame {
    pub(super) fn new(
        inner: Box<dyn ReadHalf>,
        gate: Arc<Ingress>,
        bytes: &[u8],
    ) -> zbus::Result<Self> {
        let mut frame = Self {
            inner: Some(inner),
            gate,
            primary: [0; 16],
            filled: bytes.len().min(16),
            admitted: false,
            returned: false,
            empty_close_eof: false,
        };
        frame.gate.partial.fetch_add(1, Ordering::Relaxed);
        frame.primary[..frame.filled].copy_from_slice(&bytes[..frame.filled]);
        if frame.filled == 16 {
            frame.check()?;
        }
        Ok(frame)
    }
    fn check(&mut self) -> zbus::Result<()> {
        let word = |offset: usize| {
            let bytes = self.primary[offset..offset + 4].try_into().unwrap();
            match self.primary[0] {
                b'l' => Some(u32::from_le_bytes(bytes) as usize),
                b'B' => Some(u32::from_be_bytes(bytes) as usize),
                _ => None,
            }
        };
        let fields = word(12).ok_or(zbus::Error::ExcessData)?;
        let body = word(4).ok_or(zbus::Error::ExcessData)?;
        let header = 16usize.checked_add(fields).ok_or(zbus::Error::ExcessData)?;
        let total = header
            .checked_add((8 - header % 8) % 8)
            .and_then(|n| n.checked_add(body))
            .ok_or(zbus::Error::ExcessData)?;
        if fields > FIELDS
            || total > FRAME
            || !(1..=4).contains(&self.primary[1])
            || self.primary[3] != 1
        {
            return Err(zbus::Error::ExcessData);
        }
        self.gate.admit(total);
        self.admitted = true;
        Ok(())
    }
}
impl Drop for Frame {
    fn drop(&mut self) {
        if !self.returned {
            self.gate.refuse();
        }
        self.gate.partial.fetch_sub(1, Ordering::Relaxed);
    }
}
#[async_trait]
impl ReadHalf for Frame {
    // Intentionally use SDK DEFAULT receive_message, never unsafe from_bytes.
    async fn recvmsg(&mut self, buffer: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
        if self.gate.refused() {
            return Err(self.gate.refuse());
        }
        let inner = self.inner.as_mut().ok_or_else(|| self.gate.refuse())?;
        if !self.admitted {
            let start = self.filled;
            while self.filled < 16 {
                let (n, fds) = match inner.recvmsg(&mut self.primary[self.filled..]).await {
                    Ok(value) => value,
                    Err(error)
                        if self.filled == 0
                            && self.gate.removals_acked.load(Ordering::Acquire)
                            && self.gate.close_started.load(Ordering::Acquire)
                            && sdk_empty_eof(&error) =>
                    {
                        self.empty_close_eof = true;
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "notification input closed",
                        ));
                    }
                    Err(_) => return Err(self.gate.refuse()),
                };
                if !fds.is_empty() {
                    return Err(self.gate.refuse());
                }
                if n == 0 {
                    // Separate trait-fixture branch. The locked concrete SDK
                    // half represents actual EOF with the error above.
                    if self.filled == 0
                        && self.gate.removals_acked.load(Ordering::Acquire)
                        && self.gate.close_started.load(Ordering::Acquire)
                    {
                        self.empty_close_eof = true;
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "notification input closed",
                        ));
                    }
                    return Err(self.gate.refuse());
                }
                self.filled += n;
            }
            self.check().map_err(|_| self.gate.refuse())?;
            let count = 16 - start;
            if buffer.len() < count {
                return Err(self.gate.refuse());
            }
            buffer[..count].copy_from_slice(&self.primary[start..]);
            // Only NOW does default receive_message see the complete primary,
            // so its subsequent resize cannot request an unadmitted body.
            Ok((count, vec![]))
        } else {
            if buffer.len() > FRAME - 16 {
                return Err(self.gate.refuse());
            }
            let (n, fds) = inner
                .recvmsg(buffer)
                .await
                .map_err(|_| self.gate.refuse())?;
            if n == 0 || !fds.is_empty() {
                return Err(self.gate.refuse());
            }
            Ok((n, vec![]))
        }
    }
}
impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[owned bounded frame]")
    }
}
