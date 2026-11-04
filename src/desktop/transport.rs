//! Reuse authenticated host pipes on Windows; same-UID sockets on Unix.
use super::store::Location;
use anyhow::Result;
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;

pub const TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(unix)]
mod native {
    use super::*;
    use anyhow::ensure;
    use std::{
        fs, io,
        net::Shutdown,
        os::unix::{
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            io::AsRawFd,
            net::{UnixListener, UnixStream},
        },
        path::PathBuf,
    };
    pub type Stream = UnixStream;
    pub struct Listener {
        socket: UnixListener,
        path: PathBuf,
    }
    pub fn authenticate(stream: &Stream) -> Result<()> {
        #[cfg(target_os = "linux")]
        let uid = {
            let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
            let mut size = std::mem::size_of_val(&credentials) as libc::socklen_t;
            // SAFETY: live socket and correctly sized output storage.
            ensure!(
                unsafe {
                    libc::getsockopt(
                        stream.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_PEERCRED,
                        &mut credentials as *mut _ as *mut _,
                        &mut size,
                    )
                } == 0,
                "cannot verify desktop peer"
            );
            credentials.uid
        };
        #[cfg(not(target_os = "linux"))]
        let uid = {
            let (mut uid, mut gid) = (0, 0);
            // SAFETY: live socket and valid output pointers.
            ensure!(
                unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } == 0,
                "cannot verify desktop peer"
            );
            uid
        };
        ensure!(
            uid == unsafe { libc::geteuid() },
            "desktop peer belongs to another user"
        );
        Ok(())
    }
    pub fn peer_pid(stream: &Stream) -> Result<u32> {
        authenticate(stream)?;
        #[cfg(target_os = "linux")]
        {
            let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
            let mut size = std::mem::size_of_val(&credentials) as libc::socklen_t;
            // SAFETY: live socket and exactly sized writable credentials.
            ensure!(
                unsafe {
                    libc::getsockopt(
                        stream.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_PEERCRED,
                        (&mut credentials as *mut libc::ucred).cast(),
                        &mut size,
                    )
                } == 0
                    && size as usize == std::mem::size_of_val(&credentials)
                    && credentials.pid > 0,
                "cannot verify desktop peer process"
            );
            Ok(credentials.pid as u32)
        }
        #[cfg(target_os = "macos")]
        {
            let mut pid: libc::pid_t = 0;
            let mut size = std::mem::size_of_val(&pid) as libc::socklen_t;
            // SAFETY: connected Unix socket and one writable pid_t.
            ensure!(
                unsafe {
                    libc::getsockopt(
                        stream.as_raw_fd(),
                        libc::SOL_LOCAL,
                        libc::LOCAL_PEERPID,
                        (&mut pid as *mut libc::pid_t).cast(),
                        &mut size,
                    )
                } == 0
                    && size as usize == std::mem::size_of_val(&pid)
                    && pid > 0,
                "cannot verify desktop peer process"
            );
            Ok(pid as u32)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        anyhow::bail!("desktop peer process identity is unavailable")
    }
    fn path(location: &Location) -> Result<PathBuf> {
        let path = location.root.join("service.sock");
        ensure!(
            path.as_os_str().as_encoded_bytes().len() < 104,
            "desktop socket path is too long"
        );
        Ok(path)
    }
    fn verify(path: &std::path::Path) -> Result<()> {
        let meta = fs::symlink_metadata(path)?;
        ensure!(
            meta.file_type().is_socket()
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.mode() & 0o077 == 0,
            "desktop endpoint must be a private owned socket"
        );
        Ok(())
    }
    impl Listener {
        pub fn bind(location: &Location) -> Result<Self> {
            let path = path(location)?;
            match fs::symlink_metadata(&path) {
                Ok(_) => {
                    verify(&path)?;
                    fs::remove_file(&path)?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
            // The enclosing desktop directory is verified 0700 before bind,
            // and the service's 0077 umask keeps the pre-chmod socket private.
            let socket = UnixListener::bind(&path)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            socket.set_nonblocking(true)?;
            Ok(Self { socket, path })
        }
        pub fn accept(&self) -> io::Result<Stream> {
            let (stream, _) = self.socket.accept()?;
            // BSD/macOS accept inherits O_NONBLOCK; reads/writes below use
            // blocking operations with explicit timeouts on every platform.
            stream.set_nonblocking(false)?;
            Ok(stream)
        }
        pub fn close(&self) {}
    }
    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }
    pub fn connect(location: &Location) -> Result<Option<Stream>> {
        let path = path(location)?;
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            result => {
                result?;
            }
        }
        verify(&path)?;
        let stream = match Stream::connect(path) {
            Ok(stream) => stream,
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        authenticate(&stream)?;
        Ok(Some(stream))
    }
    pub fn shutdown(stream: &Stream) {
        let _ = stream.shutdown(Shutdown::Both);
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use crate::host::{pipe, token};
    use std::io;
    pub type Stream = pipe::PipeStream;
    pub struct Listener(pipe::PipeListener);
    fn name(location: &Location) -> Result<String> {
        Ok(format!(
            r"\\.\pipe\hgs-desktop-{}-{}",
            token::user_sid()?,
            location.configuration
        ))
    }
    pub fn authenticate(stream: &Stream) -> Result<()> {
        stream
            .authenticate(&token::Identity::current()?)
            .map_err(|_| anyhow::anyhow!("cannot verify desktop peer"))?;
        Ok(())
    }
    pub fn peer_pid(stream: &Stream) -> Result<u32> {
        let peer = stream
            .authenticate(&token::Identity::current()?)
            .map_err(|_| anyhow::anyhow!("cannot verify desktop peer process"))?;
        anyhow::ensure!(peer.pid != 0, "invalid desktop peer process");
        Ok(peer.pid)
    }
    impl Listener {
        pub fn bind(location: &Location) -> Result<Self> {
            Ok(Self(pipe::PipeListener::bind(
                &name(location)?,
                &token::Identity::current()?,
            )?))
        }
        pub fn accept(&self) -> io::Result<Stream> {
            self.0.accept()
        }
        pub fn close(&self) {
            self.0.close();
        }
    }
    pub fn connect(location: &Location) -> Result<Option<Stream>> {
        match pipe::connect(&name(location)?, Instant::now() + TIMEOUT) {
            Ok(stream) => Ok(Some(stream)),
            Err(pipe::ConnectError::NoHost) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    pub fn shutdown(stream: &Stream) {
        stream.shutdown();
    }
}
pub use native::*;
