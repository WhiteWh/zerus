use super::*;

#[test]
fn line_shape_and_limit_count_the_original_final_newline() {
    assert!(frame_valid(b"{}\n"));
    assert!(!frame_valid(b""));
    assert!(!frame_valid(b"\n"));
    assert!(!frame_valid(b"{}"));
    assert!(!frame_valid(b"{}\n{}\n"));
    let mut exact = vec![b'x'; LINE_BYTES - 1];
    exact.push(b'\n');
    assert!(frame_valid(&exact));
    exact.insert(0, b'x');
    assert!(!frame_valid(&exact));
}

#[cfg(unix)]
mod sockets {
    use super::*;
    use std::{
        io::{Read, Write},
        os::unix::{io::AsRawFd, net::UnixStream},
        sync::mpsc,
        thread,
    };
    fn until() -> Instant {
        Instant::now() + Duration::from_secs(1)
    }

    #[test]
    fn exact_bound_is_accepted_in_both_directions_and_one_extra_byte_refuses() {
        let mut bytes = vec![b'x'; LINE_BYTES - 1];
        bytes.push(b'\n');
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let sent = bytes.clone();
        let task = thread::spawn(move || {
            peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            peer.set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            peer.write_all(&sent).unwrap();
            let mut read = vec![0; LINE_BYTES];
            peer.read_exact(&mut read).unwrap();
            read
        });
        let mut reader = BufReader::new(stream);
        assert_eq!(line(&mut reader, until()).unwrap().unwrap(), bytes);
        write_line(reader.get_mut(), &bytes, until()).unwrap();
        assert_eq!(task.join().unwrap(), bytes);
        let (stream, mut peer) = UnixStream::pair().unwrap();
        bytes.insert(0, b'x');
        let task = thread::spawn(move || {
            peer.set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let _ = peer.write_all(&bytes);
        });
        assert!(line(&mut BufReader::new(stream), until()).is_err());
        task.join().unwrap();
    }

    #[test]
    fn following_buffered_frame_is_retained_and_expired_data_is_not_consumed() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        peer.write_all(b"first\nsecond\n").unwrap();
        let mut reader = BufReader::new(stream);
        assert_eq!(line(&mut reader, until()).unwrap().unwrap(), b"first\n");
        assert_eq!(reader.buffer(), b"second\n");
        assert!(line(&mut reader, Instant::now() - Duration::from_nanos(1)).is_err());
        assert_eq!(reader.buffer(), b"second\n");
        assert_eq!(line(&mut reader, until()).unwrap().unwrap(), b"second\n");
        assert!(reader.buffer().is_empty());
    }

    #[test]
    fn clean_eof_and_partial_frame_are_not_confused() {
        let (stream, peer) = UnixStream::pair().unwrap();
        drop(peer);
        #[cfg(target_vendor = "apple")]
        assert_eq!(
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EINVAL),
            "exercise XNU's shutdown socket-option refusal"
        );
        assert!(line(&mut BufReader::new(stream), until())
            .unwrap()
            .is_none());
        let (stream, mut peer) = UnixStream::pair().unwrap();
        peer.write_all(b"partial").unwrap();
        drop(peer);
        assert!(line(&mut BufReader::new(stream), until()).is_err());
    }

    #[test]
    fn shutdown_timeout_mapping_is_apple_only_and_preserves_other_errors() {
        assert!(receive_timeout_result(Ok(()), true).is_ok());
        assert!(
            receive_timeout_result(Err(io::Error::from_raw_os_error(libc::EINVAL)), true).is_ok()
        );
        for (apple, errno) in [
            (false, libc::EINVAL),
            (true, libc::EBADF),
            (true, libc::EINTR),
            (true, libc::EIO),
        ] {
            assert_eq!(
                receive_timeout_result(Err(io::Error::from_raw_os_error(errno)), apple)
                    .unwrap_err()
                    .raw_os_error(),
                Some(errno)
            );
        }
        for kind in [io::ErrorKind::TimedOut, io::ErrorKind::InvalidInput] {
            assert_eq!(
                receive_timeout_result(Err(kind.into()), true)
                    .unwrap_err()
                    .kind(),
                kind
            );
        }
    }

    #[test]
    fn complete_final_frame_is_drained_before_closed_peer_eof() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        peer.write_all(b"final\n").unwrap();
        drop(peer);
        let mut reader = BufReader::new(stream);
        assert_eq!(line(&mut reader, until()).unwrap().unwrap(), b"final\n");
        assert!(line(&mut reader, until()).unwrap().is_none());
    }

    #[test]
    fn partial_progress_never_renews_the_total_read_deadline() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let (ready, started) = mpsc::sync_channel(1);
        let task = thread::spawn(move || {
            peer.write_all(b"x").unwrap();
            ready.send(()).unwrap();
            for _ in 0..100 {
                thread::sleep(Duration::from_millis(25));
                if peer.write_all(b"x").is_err() {
                    break;
                }
            }
        });
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut reader = BufReader::new(stream);
        let began = Instant::now();
        assert!(line(&mut reader, began + Duration::from_millis(120)).is_err());
        assert!(began.elapsed() < Duration::from_secs(1));
        drop(reader);
        task.join().unwrap();
    }

    #[test]
    fn blocked_partial_write_cannot_extend_its_deadline() {
        let (mut stream, _peer) = UnixStream::pair().unwrap();
        let size: libc::c_int = 1024;
        // SAFETY: the test owns this live socket; size is correctly typed and
        // readable for exactly its declared length. No owner socket is used.
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    &size as *const _ as *const _,
                    std::mem::size_of_val(&size) as libc::socklen_t,
                )
            },
            0
        );
        stream.set_nonblocking(true).unwrap();
        let chunk = [b'x'; 4096];
        let mut filled = false;
        for _ in 0..256 {
            match stream.write(&chunk) {
                Ok(_) => (),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    filled = true;
                    break;
                }
                Err(error) => panic!("private socket fill failed: {error:?}"),
            }
        }
        assert!(filled);
        stream.set_nonblocking(false).unwrap();
        let began = Instant::now();
        let mut frame = vec![b'x'; LINE_BYTES - 1];
        frame.push(b'\n');
        assert!(write_line(&mut stream, &frame, began + Duration::from_millis(100)).is_err());
        assert!(began.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn invalid_or_unbounded_write_refuses_before_any_payload_is_sent() {
        let (mut stream, mut peer) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        let mut byte = [0];
        for (frame, deadline) in [
            (&b"missing LF"[..], until()),
            (&b"first\nsecond\n"[..], until()),
            (&b"ok\n"[..], Instant::now() - Duration::from_nanos(1)),
            (&b"ok\n"[..], Instant::now() + Duration::from_secs(3600)),
        ] {
            assert!(write_line(&mut stream, frame, deadline).is_err());
            assert!(matches!(peer.read(&mut byte), Err(e) if e.kind()==io::ErrorKind::WouldBlock));
        }
    }
}
