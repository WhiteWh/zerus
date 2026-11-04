use super::test_bus as bus;
use super::*;
use std::future::Future;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};
use std::time::Duration;
use zbus::zvariant::Endian;

#[derive(Debug, Default)]
struct Observed {
    requests: Mutex<Vec<usize>>,
    consumed: AtomicUsize,
    dropped: AtomicBool,
}
#[derive(Debug)]
struct Fragmented {
    bytes: Vec<u8>,
    chunk: usize,
    observed: Arc<Observed>,
}
impl Drop for Fragmented {
    fn drop(&mut self) {
        self.observed.dropped.store(true, Ordering::Release);
    }
}
#[async_trait]
impl ReadHalf for Fragmented {
    async fn recvmsg(&mut self, buffer: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
        self.observed.requests.lock().unwrap().push(buffer.len());
        let position = self.observed.consumed.load(Ordering::Relaxed);
        if position == self.bytes.len() {
            return std::future::pending().await;
        }
        let count = buffer
            .len()
            .min(self.chunk)
            .min(self.bytes.len() - position);
        buffer[..count].copy_from_slice(&self.bytes[position..position + count]);
        self.observed.consumed.fetch_add(count, Ordering::Relaxed);
        Ok((count, vec![]))
    }
}
fn reader(bytes: Vec<u8>, chunk: usize) -> (GatedRead, Arc<Ingress>, Arc<Observed>) {
    let gate = Arc::new(Ingress::default());
    let observed = Arc::new(Observed::default());
    let read = GatedRead {
        inner: Some(Box::new(Fragmented {
            bytes,
            chunk,
            observed: observed.clone(),
        })),
        gate: gate.clone(),
        expected: None,
        line: Vec::with_capacity(AUTH),
    };
    (read, gate, observed)
}
fn word(bytes: &[u8], offset: usize) -> usize {
    let word = bytes[offset..offset + 4].try_into().unwrap();
    match bytes[0] {
        b'l' => u32::from_le_bytes(word) as usize,
        b'B' => u32::from_be_bytes(word) as usize,
        _ => panic!("fixture endian"),
    }
}
fn set_word(bytes: &mut [u8], offset: usize, value: u32) {
    let word = match bytes[0] {
        b'l' => value.to_le_bytes(),
        b'B' => value.to_be_bytes(),
        _ => panic!("fixture endian"),
    };
    bytes[offset..offset + 4].copy_from_slice(&word);
}
fn message(endian: Endian, payload: usize, path: &str) -> zbus::Message {
    zbus::Message::signal(path, "org.example.Ingress", "Boundary")
        .unwrap()
        .endian(endian)
        .build(&vec![0u8; payload])
        .unwrap()
}
fn boundary(endian: Endian) -> zbus::Message {
    let overhead = message(endian, 0, "/org/example/Ingress").data().len();
    let msg = message(endian, FRAME - overhead, "/org/example/Ingress");
    assert_eq!(msg.data().len(), FRAME);
    msg
}

#[test]
fn fragmented_default_decoder_accepts_both_endians_at_full_frame_boundary() {
    for endian in [Endian::Little, Endian::Big] {
        for chunk in [1, 7, 16, FRAME] {
            let msg = boundary(endian);
            let (mut read, gate, observed) = reader(msg.data().to_vec(), chunk);
            let decoded =
                async_io::block_on(read.receive_message(7, &mut vec![], &mut vec![])).unwrap();
            assert_eq!(&decoded.data()[..], &msg.data()[..]);
            assert_eq!(gate.admitted.load(Ordering::Relaxed), 1);
            assert_eq!(gate.peak.load(Ordering::Relaxed), FRAME);
            assert_eq!(gate.partial.load(Ordering::Relaxed), 0);
            assert_eq!(observed.consumed.load(Ordering::Relaxed), FRAME);
            assert!(!gate.refused());
            assert_eq!(observed.requests.lock().unwrap()[0], 16);
        }
    }
}

#[test]
fn invalid_primary_refuses_before_sdk_resize_or_any_body_read() {
    for endian in [Endian::Little, Endian::Big] {
        for bad in 0..5 {
            let mut bytes = boundary(endian).data()[..16].to_vec();
            match bad {
                0 => {
                    let body = word(&bytes, 4);
                    set_word(&mut bytes, 4, body as u32 + 1);
                }
                1 => set_word(&mut bytes, 12, FIELDS as u32 + 1),
                2 => bytes[0] = b'x',
                3 => bytes[1] = 0,
                4 => bytes[3] = 2,
                _ => unreachable!(),
            }
            for prebuffer in [false, true] {
                let (mut read, gate, observed) =
                    reader(if prebuffer { vec![] } else { bytes.clone() }, 1);
                let mut buffered = if prebuffer { bytes.clone() } else { vec![] };
                let error = async_io::block_on(read.receive_message(0, &mut buffered, &mut vec![]))
                    .unwrap_err();
                assert!(matches!(error, zbus::Error::ExcessData));
                assert!(gate.refused());
                assert_eq!(gate.admitted.load(Ordering::Relaxed), 0);
                assert_eq!(gate.partial.load(Ordering::Relaxed), 0);
                assert_eq!(buffered.capacity(), 0);
                assert_eq!(
                    observed.consumed.load(Ordering::Relaxed),
                    if prebuffer { 0 } else { 16 }
                );
                assert!(observed.requests.lock().unwrap().iter().all(|n| *n <= 16));
            }
        }
    }
}

#[test]
fn prebuffer_capacity_and_fd_refuse_before_default_and_drop_original_buffers() {
    for kind in 0..3 {
        let (mut read, gate, observed) = reader(vec![], 1);
        let mut bytes = if kind == 0 {
            vec![0; AUTH + 1]
        } else {
            Vec::with_capacity(AUTH + 1)
        };
        let mut fds = vec![];
        if kind == 2 {
            bytes = vec![];
            fds.push(std::fs::File::open("/dev/null").unwrap().into());
        }
        assert!(matches!(
            async_io::block_on(read.receive_message(0, &mut bytes, &mut fds)),
            Err(zbus::Error::ExcessData)
        ));
        assert!(gate.refused());
        assert_eq!(observed.consumed.load(Ordering::Relaxed), 0);
        assert!(fds.is_empty());
        assert_eq!(bytes.capacity(), 0);
    }
}

#[test]
fn cancellation_destroys_exact_moved_half_and_partial_frame_without_restoring_it() {
    for prefix in [1, 15, 16, 64] {
        let bytes = boundary(Endian::Little).data()[..prefix].to_vec();
        let (mut read, gate, observed) = reader(bytes, 1);
        let mut buffered = Vec::with_capacity(AUTH);
        let mut fds = vec![];
        let mut future = Box::pin(read.receive_message(0, &mut buffered, &mut fds));
        assert!(future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending());
        assert_eq!(gate.partial.load(Ordering::Relaxed), 1);
        assert!(!observed.dropped.load(Ordering::Acquire));
        drop(future);
        assert!(read.inner.is_none());
        assert!(observed.dropped.load(Ordering::Acquire));
        assert!(gate.refused());
        assert_eq!(gate.partial.load(Ordering::Relaxed), 0);
        assert_eq!(buffered.capacity(), 0);
        assert!(matches!(
            async_io::block_on(read.receive_message(1, &mut buffered, &mut fds)),
            Err(zbus::Error::ExcessData)
        ));
    }
}

#[test]
fn safe_sdk_decode_failure_is_scalar_before_socket_reader_retention() {
    let mut bytes = boundary(Endian::Little).data().to_vec();
    // Invalid field variant, admitted primary but SDK rejects actual metadata.
    bytes[17] = 255;
    let (mut read, gate, _) = reader(bytes, FRAME);
    let result = async_io::block_on(read.receive_message(0, &mut vec![], &mut vec![]));
    assert!(matches!(result, Err(zbus::Error::ExcessData)));
    assert!(gate.refused());
    assert_eq!(gate.partial.load(Ordering::Relaxed), 0);
}

#[test]
fn complete_auth_lines_use_sdk_whitespace_and_guid_rules_before_final_lf() {
    for line in [
        "OK 0123456789abcdef0123456789abcdef\r\n",
        " \tOK\t0123456789abcdef0123456789abcdef extra\r\n",
        "OK\x0c0123456789abcdef0123456789abcdef\r\n",
        "OK\r\n",
        "OK invalid\r\n",
        "\n",
        "DATA 00\n",
    ] {
        let (mut read, gate, _) = reader(line.as_bytes().to_vec(), 1);
        read.expected = Some(
            zbus::Guid::try_from("ffffffffffffffffffffffffffffffff")
                .unwrap()
                .to_owned(),
        );
        let mut count = 0;
        let mut buffer = [0; 1024];
        while async_io::block_on(read.recvmsg(&mut buffer)).is_ok() {
            count += 1;
        }
        assert!(count < line.len(), "final LF must never reach SDK");
        assert!(gate.refused());
    }
    let line = b" \tOK\t0123456789abcdef0123456789abcdef extra\r\n";
    let (mut read, gate, _) = reader(line.to_vec(), 1);
    read.expected = Some(
        zbus::Guid::try_from("0123456789abcdef0123456789abcdef")
            .unwrap()
            .to_owned(),
    );
    for byte in line {
        let mut buffer = [0; 1024];
        assert_eq!(async_io::block_on(read.recvmsg(&mut buffer)).unwrap().0, 1);
        assert_eq!(buffer[0], *byte);
    }
    assert!(!gate.refused());
    assert!(read.line.is_empty());
    assert_eq!(read.line.capacity(), AUTH);
}

#[test]
fn auth_cumulative_cap_and_lf_after_a_prior_line_never_overread() {
    for bytes in [vec![b'x'; AUTH + 1], b"DATA 00\r\n\n".to_vec()] {
        let limit = bytes.len().min(AUTH);
        let (mut read, gate, observed) = reader(bytes, 1024);
        let mut delivered = 0;
        let mut buffer = [0; 1024];
        while async_io::block_on(read.recvmsg(&mut buffer)).is_ok() {
            delivered += 1;
        }
        assert!(delivered <= limit);
        assert!(observed.consumed.load(Ordering::Relaxed) <= AUTH);
        assert!(read.line.capacity() <= AUTH);
        assert!(gate.refused());
        assert!(observed.requests.lock().unwrap().iter().all(|n| *n == 1));
    }
}

#[test]
fn actual_socket_fd_refuses_during_auth_primary_body_and_outgoing() {
    for phase in 0..4 {
        let (left, right) = UnixStream::pair().unwrap();
        let left = async_io::Async::new(left).unwrap();
        let right = async_io::Async::new(right).unwrap();
        let (mut socket, gate) = wrap(BoxedSplit::from(left), None);
        assert!(!socket.read().can_pass_unix_fd());
        assert!(!socket.write().can_pass_unix_fd());
        let fd = std::fs::File::open("/dev/null").unwrap();
        if phase == 3 {
            assert!(async_io::block_on(socket.write_mut().sendmsg(b"x", &[fd.as_fd()])).is_err());
        } else {
            let mut sender = BoxedSplit::from(right);
            let msg = boundary(Endian::Little);
            if phase == 2 {
                assert_eq!(
                    async_io::block_on(sender.write_mut().sendmsg(&msg.data()[..16], &[])).unwrap(),
                    16
                );
            }
            async_io::block_on(sender.write_mut().sendmsg(
                if phase == 2 {
                    &msg.data()[16..17]
                } else {
                    b"x"
                },
                &[fd.as_fd()],
            ))
            .unwrap();
            if phase == 0 {
                assert!(async_io::block_on(socket.read_mut().recvmsg(&mut [0; 1024])).is_err());
            } else {
                assert!(matches!(
                    async_io::block_on(socket.read_mut().receive_message(
                        0,
                        &mut vec![],
                        &mut vec![]
                    )),
                    Err(zbus::Error::ExcessData)
                ));
            }
        }
        assert!(gate.refused());
    }
}

#[test]
fn actual_sdk_external_wrong_guid_refuses_before_begin_and_hello() {
    for ending in [
        b"\n".as_slice(),
        b" \tOK\t0123456789abcdef0123456789abcdef extra\r\n",
        b"OK 0123456789abcdef0123456789abcdef\r\n",
    ] {
        let (left, mut right) = UnixStream::pair().unwrap();
        right
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let server = std::thread::spawn(move || {
            let mut auth = Vec::new();
            loop {
                let mut byte = [0];
                right.read_exact(&mut byte).unwrap();
                auth.push(byte[0]);
                if auth.ends_with(b"\r\n") {
                    break;
                }
            }
            assert!(auth.starts_with(b"\0AUTH EXTERNAL "));
            right.write_all(ending).unwrap();
            let mut after = Vec::new();
            right.read_to_end(&mut after).unwrap();
            assert!(after.is_empty(), "no BEGIN/Hello may be issued");
        });
        let (socket, gate) = wrap(
            BoxedSplit::from(async_io::Async::new(left).unwrap()),
            Some(
                zbus::Guid::try_from("ffffffffffffffffffffffffffffffff")
                    .unwrap()
                    .to_owned(),
            ),
        );
        assert!(async_io::block_on(
            zbus::connection::Builder::socket(socket)
                .internal_executor(false)
                .build()
        )
        .is_err());
        assert!(gate.refused());
        server.join().unwrap();
    }
}

#[test]
fn actual_private_bus_preserves_external_identity_and_expected_guid() {
    let bus = bus::Bus::new();
    async_io::block_on(async {
        let (socket, gate) = connect(bus.address.clone()).await.unwrap();
        assert_eq!(
            socket.read().auth_mechanism(),
            zbus::connection::AuthMechanism::External
        );
        let connection = zbus::connection::Builder::socket(socket)
            .internal_executor(false)
            .max_queued(16)
            .build()
            .await
            .unwrap();
        assert!(connection.unique_name().is_some());
        assert!(!gate.refused());
        assert!(gate.auth.load(Ordering::Relaxed) < AUTH);
        connection.close().await.unwrap();
    });
}

#[test]
fn retained_wire_text_table_fits_original_fixed_group_reservation() {
    const WIRE: usize = (16 + 1 + 1 + 1 + 1 + 2 + 1 + 1) * FRAME;
    const SUPPLEMENT: usize = 20 * AUTH + 16384 + 4 * AUTH + 256 + 8192 + 4096;
    assert_eq!(WIRE + SUPPLEMENT, 1995008);
    const { assert!(WIRE + SUPPLEMENT < RESERVATION) };
    // Admission/partial counts above are observations, never heap/destruction proof.
}

#[test]
fn actual_socket_full_frames_and_exact_header_limit_use_the_default_decoder() {
    for endian in [Endian::Little, Endian::Big] {
        let mut header_boundary = None;
        for length in 3900..4100 {
            let path = format!("/{}", "a".repeat(length));
            let msg = message(endian, 0, &path);
            if word(msg.data(), 12) == FIELDS {
                header_boundary = Some(msg);
                break;
            }
        }
        for msg in [
            boundary(endian),
            header_boundary.expect("exact fields fixture"),
        ] {
            let expected = msg.data().to_vec();
            let (left, mut right) = UnixStream::pair().unwrap();
            let sender = std::thread::spawn(move || {
                // Distinct fragmented primary and body writes on a real socket.
                for fragment in expected.chunks(11) {
                    right.write_all(fragment).unwrap();
                }
            });
            let (mut socket, gate) =
                wrap(BoxedSplit::from(async_io::Async::new(left).unwrap()), None);
            let decoded = async_io::block_on(socket.read_mut().receive_message(
                4,
                &mut vec![],
                &mut vec![],
            ))
            .unwrap();
            assert_eq!(&decoded.data()[..], &msg.data()[..]);
            assert_eq!(gate.peak.load(Ordering::Relaxed), msg.data().len());
            assert_eq!(gate.partial.load(Ordering::Relaxed), 0);
            assert!(!gate.refused());
            sender.join().unwrap();
        }
    }
}

#[test]
fn explicit_abstract_connector_and_real_peer_credentials_are_preserved() {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixListener};
    let name = format!("zerus-ingress-{}", uuid::Uuid::new_v4());
    let listener =
        UnixListener::bind_addr(&SocketAddr::from_abstract_name(name.as_bytes()).unwrap()).unwrap();
    let (mut socket, gate) = async_io::block_on(connect(format!("unix:abstract={name}"))).unwrap();
    let (_peer, _) = listener.accept().unwrap();
    let read_credentials = async_io::block_on(socket.read_mut().peer_credentials()).unwrap();
    let write_credentials = async_io::block_on(socket.write_mut().peer_credentials()).unwrap();
    // Kernel credentials, no synthetic UID or authenticated_socket bypass.
    assert_eq!(
        read_credentials.unix_user_id(),
        Some(unsafe { libc::geteuid() })
    );
    assert_eq!(
        write_credentials.unix_user_id(),
        read_credentials.unix_user_id()
    );
    assert!(!gate.refused());
    for address in [
        "unix:dir=/not-contacted",
        "unix:tmpdir=/not-contacted",
        "tcp:host=127.0.0.1,port=1",
    ] {
        assert!(async_io::block_on(connect(address.into())).is_err());
    }
}

#[test]
fn real_bus_saturates_one_shared16_queue_with_boundary_frames_and_drains_both_readers() {
    use std::pin::{pin, Pin};
    use std::time::Instant;
    use zbus::export::futures_core::Stream;
    let bus = bus::Bus::new();
    let sender = bus.connect();
    let (socket, ingress) = async_io::block_on(connect(bus.address.clone())).unwrap();
    let connection = async_io::block_on(
        zbus::connection::Builder::socket(socket)
            .internal_executor(false)
            .max_queued(16)
            .build(),
    )
    .unwrap();
    let executor = connection.executor().clone();
    let mut readers = [
        zbus::MessageStream::from(&connection),
        zbus::MessageStream::from(&connection),
    ];
    assert!(readers.iter().all(|r| r.max_queued() == 16));
    let tick = || {
        let tick = executor.tick();
        let _ = pin!(tick).poll(&mut Context::from_waker(Waker::noop()));
    };
    // Clear startup traffic through both real receivers before measuring burst.
    let end = Instant::now() + Duration::from_secs(5);
    while ingress.admitted.load(Ordering::Relaxed) < 2 {
        assert!(Instant::now() < end);
        tick();
        std::thread::yield_now();
    }
    for reader in &mut readers {
        while let Poll::Ready(Some(_)) =
            Pin::new(&mut *reader).poll_next(&mut Context::from_waker(Waker::noop()))
        {}
    }
    let initial = ingress.admitted.load(Ordering::Relaxed);
    let destination = connection.unique_name().unwrap().to_string();
    let make_message = |endian, payload| {
        zbus::Message::signal("/org/example/Ingress", "org.example.Ingress", "Boundary")
            .unwrap()
            .endian(endian)
            .sender(sender.unique_name().unwrap())
            .unwrap()
            .destination(destination.as_str())
            .unwrap()
            .build(&vec![0u8; payload])
            .unwrap()
    };
    let mut messages = Vec::new();
    for endian in [Endian::Little, Endian::Big] {
        let overhead = make_message(endian, 0).data().len();
        let msg = make_message(endian, FRAME - overhead);
        assert_eq!(msg.data().len(), FRAME);
        messages.push(msg);
    }
    // The independent fixture sender drives only its own connection.
    let producer = std::thread::spawn(move || {
        for n in 0..32 {
            sender.send(&messages[n % 2]).unwrap();
        }
        zbus::blocking::fdo::DBusProxy::new(&sender)
            .unwrap()
            .get_id()
            .unwrap();
    });
    let end = Instant::now() + Duration::from_secs(5);
    while ingress.admitted.load(Ordering::Relaxed) < initial + 17 {
        assert!(Instant::now() < end);
        tick();
        std::thread::yield_now();
    }
    // Admission happens at the 17th header; its body may still be in flight
    // from the producer thread. Wait for that frame to finish, never a tick count.
    while ingress.partial.load(Ordering::Relaxed) != 0 {
        assert!(Instant::now() < end, "the 17th frame must complete");
        tick();
        std::thread::yield_now();
    }
    for _ in 0..32 {
        tick();
        std::thread::yield_now();
    }
    assert_eq!(
        ingress.admitted.load(Ordering::Relaxed),
        initial + 17,
        "16 queued + one blocked broadcast, no next receive"
    );
    assert_eq!(ingress.partial.load(Ordering::Relaxed), 0);
    let mut counts = [0; 2];
    while counts != [32, 32] {
        assert!(
            Instant::now() < end,
            "both logical readers must see all boundary frames"
        );
        for (i, reader) in readers.iter_mut().enumerate() {
            if let Poll::Ready(Some(Ok(message))) =
                Pin::new(reader).poll_next(&mut Context::from_waker(Waker::noop()))
            {
                assert_eq!(message.data().len(), FRAME);
                counts[i] += 1;
                drop(message); // No overlap with the next arm's borrowed message.
            }
        }
        tick();
        std::thread::yield_now();
    }
    producer.join().unwrap();
    drop(readers);
    // Fixture-only lifecycle fact: these raw receivers are actually destroyed;
    // this separate connection has no match registrations/control calls.
    ingress.removals_acked.store(true, Ordering::Release);
    async_io::block_on(connection.clone().close()).unwrap();
    while !executor.is_empty() {
        assert!(Instant::now() < end);
        tick();
        std::thread::yield_now();
    }
    drop(connection);
    assert!(!ingress.refused());
    assert_eq!(ingress.peak.load(Ordering::Relaxed), FRAME);
    println!("REAL BUS: 32 boundary frames, both endians, one raw16 queue + one blocked broadcast, 64 logical deliveries, exact executor drain; wire admission observations only");
}

#[test]
fn actual_empty_close_eof_requires_removals_and_never_exempts_a_partial_frame() {
    for armed in [false, true] {
        for prefix in [0, 1, 15, 16, 64] {
            let (left, mut right) = UnixStream::pair().unwrap();
            let msg = boundary(Endian::Little);
            right.write_all(&msg.data()[..prefix]).unwrap();
            let (socket, gate) = wrap(BoxedSplit::from(async_io::Async::new(left).unwrap()), None);
            let (mut read, mut write) = socket.take();
            let mut bytes = vec![];
            let mut fds = vec![];
            let mut future = Box::pin(read.receive_message(0, &mut bytes, &mut fds));
            assert!(future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending());
            gate.removals_acked.store(armed, Ordering::Release);
            async_io::block_on(write.close()).unwrap();
            assert!(matches!(
                async_io::block_on(future),
                Err(zbus::Error::ExcessData)
            ));
            assert_eq!(gate.refused(), !armed || prefix != 0);
            assert_eq!(gate.partial.load(Ordering::Relaxed), 0);
            assert_eq!(bytes.capacity(), 0);
        }
    }
}

#[test]
fn armed_close_never_waives_cancellation_or_clears_an_earlier_refusal() {
    for failed in [false, true] {
        let (mut read, gate, observed) = reader(vec![], 1);
        gate.removals_acked.store(true, Ordering::Release);
        gate.close_started.store(true, Ordering::Release);
        if failed {
            gate.refuse();
        }
        let mut bytes = vec![];
        let mut fds = vec![];
        let mut future = Box::pin(read.receive_message(0, &mut bytes, &mut fds));
        let result = future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()));
        assert_eq!(result.is_ready(), failed);
        drop(future);
        assert!(gate.refused());
        if !failed {
            assert!(observed.dropped.load(Ordering::Acquire));
        }
        assert_eq!(gate.partial.load(Ordering::Relaxed), 0);
    }
}

#[derive(Debug)]
struct InvalidEmpty {
    fd: bool,
}
#[async_trait]
impl ReadHalf for InvalidEmpty {
    async fn recvmsg(&mut self, _: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
        if self.fd {
            Ok((0, vec![std::fs::File::open("/dev/null").unwrap().into()]))
        } else {
            Err(io::Error::other("synthetic read failure"))
        }
    }
}
#[test]
fn armed_empty_fd_or_io_failure_is_still_a_sticky_refusal() {
    for fd in [false, true] {
        let (mut read, gate, _) = reader(vec![], 1);
        read.inner = Some(Box::new(InvalidEmpty { fd }));
        gate.removals_acked.store(true, Ordering::Release);
        gate.close_started.store(true, Ordering::Release);
        assert!(matches!(
            async_io::block_on(read.receive_message(0, &mut vec![], &mut vec![])),
            Err(zbus::Error::ExcessData)
        ));
        assert!(gate.refused());
        assert_eq!(gate.partial.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn sdk_eof_literal_is_exact_and_never_accepts_raw_epipe_or_other_errors() {
    for (kind, text, expected) in [
        (
            io::ErrorKind::BrokenPipe,
            "failed to read from socket",
            true,
        ),
        (
            io::ErrorKind::BrokenPipe,
            "failed to read from socket extra",
            false,
        ),
        (
            io::ErrorKind::BrokenPipe,
            "failed to read from socke",
            false,
        ),
        (io::ErrorKind::BrokenPipe, "other", false),
        (
            io::ErrorKind::UnexpectedEof,
            "failed to read from socket",
            false,
        ),
    ] {
        assert_eq!(frame::sdk_empty_eof(&io::Error::new(kind, text)), expected);
    }
    assert!(!frame::sdk_empty_eof(&io::Error::from_raw_os_error(
        libc::EPIPE
    )));
    let long = "x".repeat(AUTH);
    assert!(!frame::sdk_empty_eof(&io::Error::new(
        io::ErrorKind::BrokenPipe,
        long
    )));
}

#[test]
fn real_sdk_max_auth_rejected_text_and_cap_refusal_never_emit_begin() {
    for over_cap in [false, true] {
        let (left, mut right) = UnixStream::pair().unwrap();
        right
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let server = std::thread::spawn(move || {
            let mut auth = Vec::new();
            while !auth.ends_with(b"\r\n") {
                let mut byte = [0];
                right.read_exact(&mut byte).unwrap();
                auth.push(byte[0]);
            }
            assert!(auth.starts_with(b"\0AUTH EXTERNAL "));
            let mut text = b"REJECTED".to_vec();
            while text.len() + 2 <= AUTH - 2 {
                text.extend_from_slice(b" x");
            }
            text.resize(AUTH - 2, b' ');
            if over_cap {
                text.extend_from_slice(b"x\r\n");
            } else {
                text.extend_from_slice(b"\r\n");
            }
            assert_eq!(text.len(), AUTH + usize::from(over_cap));
            right.write_all(&text).unwrap();
            let mut after = Vec::new();
            // A cap refusal can close with one unread byte and reset the peer.
            if let Err(error) = right.read_to_end(&mut after) {
                assert!(over_cap && error.kind() == io::ErrorKind::ConnectionReset);
            }
            assert!(after.is_empty());
        });
        let (socket, gate) = wrap(BoxedSplit::from(async_io::Async::new(left).unwrap()), None);
        assert!(async_io::block_on(
            zbus::connection::Builder::socket(socket)
                .internal_executor(false)
                .build()
        )
        .is_err());
        assert_eq!(gate.auth.load(Ordering::Relaxed), AUTH);
        assert_eq!(gate.refused(), over_cap);
        assert_eq!(gate.admitted.load(Ordering::Relaxed), 0);
        server.join().unwrap();
    }
}
