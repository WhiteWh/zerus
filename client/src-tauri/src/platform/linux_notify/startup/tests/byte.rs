use super::*;

fn reserved(gate: &Gate) -> usize {
    gate.snapshot().unwrap().count(Role::NativePosting) * ingress::RESERVATION
}

#[test]
fn reservation_precedes_start_and_requires_exact_clean_join_even_after_socket_destruction() {
    let bus = Bus::new();
    let gate = Gate::new(1).unwrap();
    let hold = Arc::new(Hold::default());
    let held = hold.clone();
    let (mut startup, control) = spawn(
        bus.address.clone(),
        gate.admit(Role::NativePosting).unwrap(),
        Hooks {
            event: Some(Arc::new(move |phase| {
                if phase == Hook::BeforeNative {
                    held.enter();
                }
            })),
            ..Hooks::default()
        },
    )
    .unwrap();
    wait(|| hold.entered.load(Ordering::Acquire));
    assert_eq!(reserved(&gate), 2097152);
    hold.release();
    wait(|| control.snapshot().unwrap().ready);
    control.close().unwrap();
    wait(|| startup.worker.as_ref().unwrap().is_finished());
    assert_eq!(reserved(&gate), 2097152); // No socket/counter/destructor refund.
    finish(&mut startup);
    assert_eq!(reserved(&gate), 0);
}

#[test]
fn real_oversized_hello_refusal_keeps_original_reservation_after_every_handle_is_lost() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    let root = std::env::temp_dir().join(format!("zib-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("bus");
    let listener = UnixListener::bind(&path).unwrap();
    let server = std::thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut auth = Vec::new();
        while !auth.ends_with(b"\r\n") {
            let mut byte = [0];
            peer.read_exact(&mut byte).unwrap();
            auth.push(byte[0]);
        }
        assert!(auth.starts_with(b"\0AUTH EXTERNAL "));
        peer.write_all(b"OK 0123456789abcdef0123456789abcdef\r\n")
            .unwrap();
        let mut begin = [0; 7];
        peer.read_exact(&mut begin).unwrap();
        assert_eq!(&begin, b"BEGIN\r\n");
        let mut hello = [0; 16];
        peer.read_exact(&mut hello).unwrap();
        assert_eq!(hello[0], b'l');
        // Only an oversized primary is sent: no payload/body may be requested.
        let mut reply = [0u8; 16];
        reply[0] = b'l';
        reply[1] = 2;
        reply[3] = 1;
        reply[4..8].copy_from_slice(&(ingress::FRAME as u32 + 1).to_le_bytes());
        reply[8..12].copy_from_slice(&1u32.to_le_bytes());
        peer.write_all(&reply).unwrap();
    });
    let gate = Gate::new(1).unwrap();
    let (mut startup, control) = spawn_owned_startup(
        format!("unix:path={}", path.display()),
        gate.admit(Role::NativePosting).unwrap(),
    )
    .unwrap();
    wait(|| startup.worker.as_ref().unwrap().is_finished());
    assert_eq!(
        startup.try_finish().unwrap_err(),
        StartupRefused::Unavailable
    );
    assert!(control.snapshot().unwrap().pinned);
    assert_eq!(reserved(&gate), 2097152);
    drop(startup);
    drop(control);
    assert_eq!(reserved(&gate), 2097152);
    assert_eq!(
        gate.close().wait_until(Instant::now()),
        Observation::Pending
    );
    server.join().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
