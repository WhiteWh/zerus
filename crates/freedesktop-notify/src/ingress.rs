//! Private byte gate shared by the original UI and service workers.
//! This file is included as a private module; lifecycle flags stay inside
//! each original worker, with no public setter or connection export.
//! The original UI E2 accounting is enumerated byte storage, not total heap.
//! F=64KiB, fields=4KiB, cumulative auth=16KiB. Reserve 2MiB under the
//! original NativePosting group: 24F + 422144 = 1995008 <= 2097152.
//! Decoded Value/Signature/QuickFields/diagnostics, allocator/queue metadata,
//! runtime/stack/FD/kernel/tracing remain a separate OPEN production gate.
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::{ffi::OsStringExt, net::UnixStream};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use zbus::connection::socket::{BoxedSplit, ReadHalf, Split, WriteHalf};
use zbus::export::async_trait::async_trait;

#[path = "ingress/frame.rs"]
mod frame;
#[cfg(test)]
#[path = "ingress/test_bus.rs"]
pub(super) mod test_bus;
#[cfg(test)]
#[path = "ingress/tests.rs"]
mod tests;
pub(super) const FRAME: usize = 65536;
pub(super) const FIELDS: usize = 4096;
pub(super) const AUTH: usize = 16384;
pub(super) const RESERVATION: usize = 2097152;

/// Admission observations, not SDK allocation/destruction or completion proof.
#[derive(Default)]
pub(super) struct Ingress {
    failed: AtomicBool,
    // Only the original worker may record successful teardown of all raw readers
    // and every removal, while its original cleanup deadline is still live.
    pub(super) removals_acked: AtomicBool,
    close_started: AtomicBool,
    pub(super) admitted: AtomicUsize,
    pub(super) peak: AtomicUsize,
    pub(super) auth: AtomicUsize,
    pub(super) partial: AtomicUsize,
}
impl Ingress {
    pub(super) fn refused(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
    fn refuse(&self) -> io::Error {
        self.failed.store(true, Ordering::Release);
        io::Error::other("notification ingress refused")
    }
    fn admit(&self, bytes: usize) {
        self.peak.fetch_max(bytes, Ordering::Relaxed);
        self.admitted.fetch_add(1, Ordering::Relaxed);
    }
}

// Only one original worker calls this under its original setup deadline.
// No environment address, autolaunch, user_id override or alternative socket.
pub(super) async fn connect(address: String) -> Result<(BoxedSplit, Arc<Ingress>), ()> {
    use zbus::address::{transport::UnixSocket, Transport};
    let parsed = zbus::Address::try_from(address.as_str()).map_err(|_| ())?;
    let expected = parsed.guid().map(|g| g.to_owned());
    let path = match parsed.transport() {
        Transport::Unix(unix) => match unix.path() {
            UnixSocket::File(path) => path.clone(),
            UnixSocket::Abstract(name) => {
                use std::os::unix::ffi::OsStrExt;
                let mut bytes = Vec::with_capacity(name.as_bytes().len() + 1);
                bytes.push(0);
                bytes.extend_from_slice(name.as_bytes());
                std::ffi::OsString::from_vec(bytes).into()
            }
            _ => return Err(()),
        },
        _ => return Err(()),
    };
    // At most four A-sized address/path copies; none follows into auth.
    drop(parsed);
    drop(address);
    let stream = async_io::Async::<UnixStream>::connect(path)
        .await
        .map_err(|_| ())?;
    Ok(wrap(BoxedSplit::from(stream), expected))
}
fn wrap(socket: BoxedSplit, expected: Option<zbus::Guid<'static>>) -> (BoxedSplit, Arc<Ingress>) {
    let gate = Arc::new(Ingress::default());
    let (read, write) = socket.take();
    let read = GatedRead {
        inner: Some(read),
        gate: gate.clone(),
        expected,
        line: Vec::with_capacity(AUTH),
    };
    let write = GatedWrite {
        inner: write,
        gate: gate.clone(),
    };
    (
        Split::new(
            Box::new(read) as Box<dyn ReadHalf>,
            Box::new(write) as Box<dyn WriteHalf>,
        ),
        gate,
    )
}
struct GatedRead {
    inner: Option<Box<dyn ReadHalf>>,
    gate: Arc<Ingress>,
    expected: Option<zbus::Guid<'static>>,
    // Sole private dynamic byte buffer; its AUTH capacity is in E2 auth lane,
    // not hidden behind a size_of pointer assertion.
    line: Vec<u8>,
}
#[async_trait]
impl ReadHalf for GatedRead {
    async fn recvmsg(&mut self, buffer: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
        if self.gate.refused()
            || buffer.is_empty()
            || self.gate.auth.load(Ordering::Relaxed) >= AUTH
        {
            return Err(self.gate.refuse());
        }
        let inner = self.inner.as_mut().ok_or_else(|| self.gate.refuse())?;
        let (n, fds) = inner
            .recvmsg(&mut buffer[..1])
            .await
            .map_err(|_| self.gate.refuse())?;
        if n != 1 || !fds.is_empty() {
            return Err(self.gate.refuse());
        }
        self.gate.auth.fetch_add(1, Ordering::Relaxed);
        let byte = buffer[0];
        // Guard every fragment BEFORE Common's lf_index-1 indexing.
        if byte == b'\n' && self.line.last() != Some(&b'\r') {
            return Err(self.gate.refuse());
        }
        self.line.push(byte); // capacity AUTH, cumulative input <=AUTH.
        if byte == b'\n' {
            let line = std::str::from_utf8(&self.line[..self.line.len() - 2])
                .map_err(|_| self.gate.refuse())?;
            // Match Command::from_str exactly: leading/alternate ASCII
            // whitespace and trailing tokens must not bypass pre-BEGIN GUID.
            let mut words = line.split_ascii_whitespace();
            if words.next() == Some("OK") {
                let value = words.next().ok_or_else(|| self.gate.refuse())?;
                let guid = zbus::Guid::try_from(value).map_err(|_| self.gate.refuse())?;
                if self
                    .expected
                    .as_ref()
                    .is_some_and(|expected| expected != &guid)
                {
                    return Err(self.gate.refuse());
                }
            }
            // Complete OK/GUID is checked BEFORE its LF reaches SDK, which
            // alone performs genuine EXTERNAL and then sends BEGIN + Hello.
            self.line.clear();
        }
        Ok((1, vec![]))
    }
    async fn receive_message(
        &mut self,
        seq: u64,
        bytes: &mut Vec<u8>,
        fds: &mut Vec<OwnedFd>,
    ) -> zbus::Result<zbus::Message> {
        if self.gate.refused() || bytes.len() > AUTH || bytes.capacity() > AUTH || !fds.is_empty() {
            fds.clear();
            *bytes = Vec::new();
            self.gate.refuse();
            return Err(zbus::Error::ExcessData);
        }
        let Some(inner) = self.inner.take() else {
            self.gate.refuse();
            return Err(zbus::Error::ExcessData);
        };
        // Move the original prebuffer, never copy it. Cancellation destroys
        // this local along with the exact moved half; the caller retains none.
        let mut buffered = std::mem::take(bytes);
        let mut frame = frame::Frame::new(inner, self.gate.clone(), &buffered)?;
        // The default SDK method retains decoding, safe Message construction
        // and sequence identity. The helper owns the EXACT moved socket half.
        let result = frame.receive_message(seq, &mut buffered, fds).await;
        if result.is_ok() {
            *bytes = buffered;
        }
        let empty_close_eof = frame.empty_close_eof;
        frame.returned = true;
        self.inner = frame.inner.take();
        // Scalar BEFORE SocketReader fail_all/closed_error/raw broadcast.
        result.map_err(|_| {
            if !empty_close_eof {
                self.gate.refuse();
            }
            zbus::Error::ExcessData
        })
    }
    async fn peer_credentials(&mut self) -> io::Result<zbus::fdo::ConnectionCredentials> {
        self.inner
            .as_mut()
            .ok_or_else(|| self.gate.refuse())?
            .peer_credentials()
            .await
    }
    fn auth_mechanism(&self) -> zbus::connection::AuthMechanism {
        self.inner
            .as_ref()
            .map(|r| r.auth_mechanism())
            .unwrap_or(zbus::connection::AuthMechanism::External)
    }
}
struct GatedWrite {
    inner: Box<dyn WriteHalf>,
    gate: Arc<Ingress>,
}
#[async_trait]
impl WriteHalf for GatedWrite {
    async fn sendmsg(&mut self, bytes: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<usize> {
        if self.gate.refused() || !fds.is_empty() || bytes.len() > FRAME {
            return Err(self.gate.refuse());
        }
        let n = self
            .inner
            .sendmsg(bytes, fds)
            .await
            .map_err(|_| self.gate.refuse())?;
        if n == 0 {
            return Err(self.gate.refuse());
        }
        Ok(n)
    }
    async fn close(&mut self) -> io::Result<()> {
        if self.gate.removals_acked.load(Ordering::Acquire) {
            self.gate.close_started.store(true, Ordering::Release);
        }
        self.inner.close().await
    }
    async fn peer_credentials(&mut self) -> io::Result<zbus::fdo::ConnectionCredentials> {
        self.inner.peer_credentials().await
    }
}
macro_rules! redacted { ($($t:ty),+) => { $(impl std::fmt::Debug for $t {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result { f.write_str("[bounded notification ingress]") }
})+ }; }
redacted!(GatedRead, GatedWrite);
// All additional private owned storage is fixed, except line.capacity()==AUTH
// (above) and address staging (dropped before auth). SDK trait objects/tasks
// and original channel runtime bookkeeping are class M, not a heap claim.
const _: () = assert!(
    std::mem::size_of::<Ingress>()
        + std::mem::size_of::<GatedRead>()
        + std::mem::size_of::<GatedWrite>()
        + std::mem::size_of::<frame::Frame>()
        < 8192
);
