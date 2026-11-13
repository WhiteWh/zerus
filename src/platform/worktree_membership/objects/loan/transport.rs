//! Host-local bounded rights transport. This checks framing/FD ownership only;
//! the closed holder must admit the original native peer and phase FIRST.
use super::{MAX_FDS, MAX_FRAME};
use std::{
    io,
    mem::{size_of, size_of_val},
    os::{
        fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
        unix::net::UnixStream,
    },
    time::Instant,
};

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid native object loan frame",
    )
}
fn check(until: Instant) -> io::Result<()> {
    if Instant::now() >= until {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "native object loan deadline",
        ))
    } else {
        Ok(())
    }
}
fn wait(stream: &UnixStream, events: libc::c_short, until: Instant) -> io::Result<()> {
    loop {
        check(until)?;
        let mut fd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events,
            revents: 0,
        };
        let millis = until
            .saturating_duration_since(Instant::now())
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let ready = unsafe { libc::poll(&mut fd, 1, millis) };
        if ready > 0 {
            if fd.revents & libc::POLLNVAL != 0 {
                return Err(invalid());
            }
            return Ok(()); // HUP/ERR is consumed by the following native read/write.
        }
        if ready < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error());
        }
    }
}
fn retry(stream: &UnixStream, events: libc::c_short, until: Instant) -> io::Result<()> {
    let error = io::Error::last_os_error();
    match error.kind() {
        io::ErrorKind::Interrupted => check(until),
        io::ErrorKind::WouldBlock => wait(stream, events, until),
        _ => Err(error),
    }
}

/// One data byte carries SCM_RIGHTS; a length-delimited bounded frame follows.
/// UNIX stream ancillary data is a barrier, not a message boundary:
/// https://man7.org/linux/man-pages/man7/unix.7.html
#[allow(clippy::unnecessary_cast)] // msghdr/cmsghdr widths differ on Darwin.
pub(crate) fn send(
    stream: &UnixStream,
    bytes: &[u8],
    fds: &[BorrowedFd<'_>],
    until: Instant,
) -> io::Result<()> {
    check(until)?;
    if bytes.len() > MAX_FRAME || fds.len() > MAX_FDS {
        return Err(invalid());
    }
    let raw: Vec<_> = fds.iter().map(AsRawFd::as_raw_fd).collect();
    let space = unsafe { libc::CMSG_SPACE(size_of_val(raw.as_slice()) as u32) } as usize;
    let mut control = vec![0usize; space.div_ceil(size_of::<usize>())];
    let marker = b'F';
    let mut iov = libc::iovec {
        iov_base: (&marker as *const u8).cast_mut().cast(),
        iov_len: 1,
    };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    if !raw.is_empty() {
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = space as _;
        // SAFETY: usize-aligned CMSG_SPACE storage, initialized header, bounded
        // native ints; all borrowed handles stay owned throughout sendmsg.
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&message);
            if header.is_null() {
                return Err(invalid());
            }
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(size_of_val(raw.as_slice()) as u32) as _;
            std::ptr::copy_nonoverlapping(
                raw.as_ptr().cast::<u8>(),
                libc::CMSG_DATA(header),
                size_of_val(raw.as_slice()),
            );
        }
    }
    loop {
        check(until)?;
        let used = unsafe {
            libc::sendmsg(
                stream.as_raw_fd(),
                &message,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if used == 1 {
            break;
        }
        if used >= 0 {
            return Err(invalid());
        }
        retry(stream, libc::POLLOUT, until)?;
    }
    let mut frame = Vec::with_capacity(4 + bytes.len());
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend_from_slice(bytes);
    let mut at = 0;
    while at < frame.len() {
        check(until)?;
        let used = unsafe {
            libc::send(
                stream.as_raw_fd(),
                frame[at..].as_ptr().cast(),
                frame.len() - at,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if used > 0 {
            at += used as usize;
        } else if used == 0 {
            return Err(invalid());
        } else {
            retry(stream, libc::POLLOUT, until)?;
        }
    }
    Ok(())
}

// Only recvmsg (or the private native-buffer test) supplies this initialized
// control buffer. Adopt every complete installed right even if Darwin reports
// a truncated header length; rejection must still close those descriptors.
#[allow(clippy::unnecessary_cast)]
fn rights(control: &mut [usize], length: usize) -> (Vec<OwnedFd>, bool) {
    let capacity = size_of_val(control);
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = length.min(capacity) as _;
    let mut fds = Vec::new();
    let mut malformed = length > capacity;
    // SAFETY: headers and complete data ints are bounded by initialized native
    // storage. All adopted rights become owned before any refusal is returned.
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            let minimum = libc::CMSG_LEN(0) as usize;
            let declared = (*header).cmsg_len as usize;
            let offset = header as usize - control.as_ptr() as usize;
            let available = (message.msg_controllen as usize).saturating_sub(offset);
            if declared < minimum || available < minimum {
                malformed = true;
                break;
            }
            let contained = declared.min(available);
            malformed |= declared > available;
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                let data = contained - minimum;
                malformed |= !data.is_multiple_of(size_of::<libc::c_int>());
                for at in 0..data / size_of::<libc::c_int>() {
                    let raw = std::ptr::read_unaligned(
                        libc::CMSG_DATA(header).cast::<libc::c_int>().add(at),
                    );
                    if raw < 0 {
                        malformed = true;
                        continue;
                    }
                    let fd = OwnedFd::from_raw_fd(raw);
                    #[cfg(target_os = "macos")]
                    if libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) != 0 {
                        malformed = true;
                        drop(fd);
                        continue;
                    }
                    let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFD);
                    if flags < 0 || flags & libc::FD_CLOEXEC == 0 {
                        malformed = true;
                        drop(fd);
                        continue;
                    }
                    fds.push(fd);
                }
            } else {
                malformed = true;
            }
            if declared > available {
                break;
            }
            header = libc::CMSG_NXTHDR(&message, header);
        }
    }
    (fds, malformed)
}

#[allow(clippy::unnecessary_cast)]
fn part(
    stream: &UnixStream,
    bytes: &mut [u8],
    until: Instant,
) -> io::Result<(usize, Vec<OwnedFd>)> {
    loop {
        check(until)?;
        let space =
            unsafe { libc::CMSG_SPACE((MAX_FDS * size_of::<libc::c_int>()) as u32) } as usize;
        let mut control = vec![0usize; space.div_ceil(size_of::<usize>())];
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = space as _;
        #[cfg(target_os = "linux")]
        let flags = libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC;
        #[cfg(target_os = "macos")]
        let flags = libc::MSG_DONTWAIT;
        // Darwin has no atomic MSG_CMSG_CLOEXEC. Keep descriptor receipt and
        // fcntl inside the same turn used by every production native spawn.
        #[cfg(target_os = "macos")]
        let turn = crate::platform::spawn_turn();
        let used = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut message, flags) };
        if used < 0 {
            #[cfg(target_os = "macos")]
            drop(turn);
            retry(stream, libc::POLLIN, until)?;
            continue;
        }
        let (fds, malformed) = rights(&mut control, message.msg_controllen as usize);
        #[cfg(target_os = "macos")]
        drop(turn);
        if malformed
            || fds.len() > MAX_FDS
            || message.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0
        {
            return Err(invalid());
        }
        if used == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "native loan peer closed",
            ));
        }
        return Ok((used as usize, fds));
    }
}
fn exact(stream: &UnixStream, mut bytes: &mut [u8], until: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        let (used, fds) = part(stream, bytes, until)?;
        if !fds.is_empty() {
            return Err(invalid());
        }
        bytes = &mut bytes[used..];
    }
    Ok(())
}
pub(crate) fn receive(stream: &UnixStream, until: Instant) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut marker = [0u8; 1];
    let (_, fds) = part(stream, &mut marker, until)?;
    if marker != *b"F" {
        return Err(invalid());
    }
    let mut size = [0u8; 4];
    exact(stream, &mut size, until)?;
    let size = u32::from_be_bytes(size) as usize;
    if size > MAX_FRAME {
        return Err(invalid());
    }
    let mut bytes = vec![0u8; size];
    exact(stream, &mut bytes, until)?;
    Ok((bytes, fds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::File, os::fd::AsFd};
    fn until() -> Instant {
        Instant::now() + std::time::Duration::from_secs(3)
    }
    /// The descriptors of this process that share `file`'s open file
    /// description (kcmp KCMP_FILE): `file` itself and every received copy of
    /// a right to it. Not every descriptor on its inode: another thread of the
    /// test process may hold a brief new open of the same file, such as the
    /// worktree membership scan's O_PATH open of each /proc/self/fd entry,
    /// which is a description of its own (zerus-yskhm).
    #[cfg(target_os = "linux")]
    fn copies(file: &File) -> usize {
        const KCMP_FILE: libc::c_int = 0;
        let pid = std::process::id() as libc::pid_t;
        let shares = |fd: libc::c_int| unsafe {
            libc::syscall(libc::SYS_kcmp, pid, pid, KCMP_FILE, file.as_raw_fd(), fd) == 0
        };
        // Without kcmp the count would prove nothing.
        assert!(
            shares(file.as_raw_fd()),
            "kcmp KCMP_FILE: {}",
            io::Error::last_os_error()
        );
        std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|fd| fd.file_name().to_str()?.parse::<libc::c_int>().ok())
            .filter(|&fd| shares(fd))
            .count()
    }
    #[test]
    fn real_rights_keep_the_same_native_file_and_close_on_exec() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        let original = tempfile::tempfile().unwrap();
        send(
            &sender,
            b"closed native object data",
            &[original.as_fd()],
            until(),
        )
        .unwrap();
        let (bytes, fds) = receive(&receiver, until()).unwrap();
        assert_eq!(bytes, b"closed native object data");
        assert_eq!(fds.len(), 1);
        let file = File::from(fds.into_iter().next().unwrap());
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            (
                file.metadata().unwrap().dev(),
                file.metadata().unwrap().ino()
            ),
            (
                original.metadata().unwrap().dev(),
                original.metadata().unwrap().ino()
            )
        );
        assert!(unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC != 0);
    }
    #[test]
    fn deadlines_closed_peers_and_oversized_frames_refuse_without_an_io_loop() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        assert!(receive(&receiver, Instant::now()).is_err());
        assert!(send(&sender, &vec![0; MAX_FRAME + 1], &[], until()).is_err());
        // Another parallel fixture may be between fork and exec with a
        // CLOEXEC copy. Shut down THIS endpoint to establish a kernel-closed
        // peer independently of that temporary duplicate; Drop alone races it.
        receiver.shutdown(std::net::Shutdown::Both).unwrap();
        drop(receiver);
        assert!(send(&sender, b"data", &[], until()).is_err());
    }
    #[test]
    fn rights_in_payload_are_rejected_and_always_closed() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        let file = tempfile::tempfile().unwrap();
        #[cfg(target_os = "linux")]
        let before = copies(&file);
        // Two transport frames nested where a plain length must be: the
        // second frame's rights must not be silently accepted or leaked.
        send(&sender, b"", &[], until()).unwrap();
        let (empty, fds) = receive(&receiver, until()).unwrap();
        assert!(empty.is_empty() && fds.is_empty());
        send(&sender, b"next", &[file.as_fd()], until()).unwrap();
        let mut byte = [0u8; 1];
        assert!(exact(&receiver, &mut byte, until()).is_err());
        assert!(file.metadata().is_ok());
        #[cfg(target_os = "linux")]
        assert_eq!(copies(&file), before, "unexpected rights leaked on refusal");
    }
    #[test]
    #[allow(clippy::unnecessary_cast)]
    fn a_truncated_native_rights_roster_refuses_and_releases_every_received_copy() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        let file = tempfile::tempfile().unwrap();
        #[cfg(target_os = "linux")]
        let before = copies(&file);
        // Bypass the sender's guard to exercise a hostile native peer. The
        // kernel closes rights which do not fit, and our RAII closes all that do.
        let rights = [file.as_raw_fd(); MAX_FDS + 2];
        let space = unsafe { libc::CMSG_SPACE(size_of_val(&rights) as u32) } as usize;
        let mut control = vec![0usize; space.div_ceil(size_of::<usize>())];
        let marker = b'F';
        let mut iov = libc::iovec {
            iov_base: (&marker as *const u8).cast_mut().cast(),
            iov_len: 1,
        };
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = space as _;
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&message);
            assert!(!header.is_null());
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(size_of_val(&rights) as u32) as _;
            std::ptr::copy_nonoverlapping(
                rights.as_ptr().cast::<u8>(),
                libc::CMSG_DATA(header),
                size_of_val(&rights),
            );
            assert_eq!(
                libc::sendmsg(sender.as_raw_fd(), &message, libc::MSG_NOSIGNAL),
                1
            );
        }
        assert!(receive(&receiver, until()).is_err());
        #[cfg(target_os = "linux")]
        assert_eq!(copies(&file), before, "truncated rights leaked on refusal");
    }
    #[test]
    #[allow(clippy::unnecessary_cast)]
    fn a_clipped_rights_header_still_adopts_and_closes_its_complete_native_fds() {
        let original = tempfile::tempfile().unwrap();
        let raw = [0; 2].map(|_| {
            let fd = unsafe { libc::fcntl(original.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
            assert!(fd >= 0);
            fd
        });
        let space = unsafe { libc::CMSG_SPACE(size_of_val(&raw) as u32) } as usize;
        let mut control = vec![0usize; space.div_ceil(size_of::<usize>())];
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = space as _;
        // Model Darwin's truncated SCM_RIGHTS header while using real private
        // duplicated descriptors. There are two complete installed rights in
        // the received bytes, even though the header declares three.
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&message);
            assert!(!header.is_null());
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN((3 * size_of::<libc::c_int>()) as u32) as _;
            std::ptr::copy_nonoverlapping(
                raw.as_ptr().cast::<u8>(),
                libc::CMSG_DATA(header),
                size_of_val(&raw),
            );
        }
        let bytes = unsafe { libc::CMSG_LEN(size_of_val(&raw) as u32) } as usize;
        let (fds, malformed) = rights(&mut control, bytes);
        assert!(malformed);
        assert_eq!(fds.len(), raw.len());
        for fd in &fds {
            assert!(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC != 0);
        }
        drop(fds);
        for fd in raw {
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstat(fd, &mut stat) } == 0 {
                use std::os::unix::fs::MetadataExt;
                let metadata = original.metadata().unwrap();
                // Parallel fixtures may already reuse a closed number. It
                // must no longer refer to this private original object.
                assert_ne!(
                    (stat.st_dev as u64, stat.st_ino as u64),
                    (metadata.dev(), metadata.ino())
                );
            }
        }
    }
}
