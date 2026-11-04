use super::*;
use serde_json::{json, Value};
use std::{
    io::{BufRead, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    sync::{mpsc, Barrier},
    thread,
    time::{Duration, Instant},
};
use zerus_client_core::desktop::{self, Build, Responder};

const SERVICE: &str = "11111111-1111-4111-8111-111111111111";
const INSTANCE: &str = "22222222-2222-4222-8222-222222222222";
const NONCE: &str = "33333333-3333-4333-8333-333333333333";
fn responder() -> Arc<Responder> {
    let responder = Arc::new(Responder::new(
        "com.hgdev.hgs-tray".into(),
        Build {
            version: "1.2.3".into(),
            source_commit: "b".repeat(40),
            source_dirty: Some(false),
        },
        INSTANCE.into(),
        true,
        false,
    ));
    assert!(responder.configure_once("a".repeat(64)));
    responder
}
fn expected() -> Hello {
    serde_json::from_slice(
        &responder()
            .reply(format!("{} {NONCE}", desktop::ABI).as_bytes())
            .unwrap(),
    )
    .unwrap()
}
fn registry() -> Registry {
    Registry::new("a".repeat(64), SERVICE.into()).unwrap()
}
fn pending(registry: &Registry) -> (Pending, UnixStream) {
    let (stream, peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    peer.set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    (
        registry.begin(BufReader::new(stream), expected()).unwrap(),
        peer,
    )
}
fn offer(peer: &mut UnixStream) -> Value {
    let mut bytes = Vec::new();
    // One byte at a time avoids a second buffered reader stealing the tail.
    loop {
        let mut byte = [0];
        peer.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
        assert!(bytes.len() <= super::super::producer_io::LINE_BYTES);
    }
    serde_json::from_slice(&bytes).unwrap()
}
fn ready(offer: &Value) -> Value {
    json!({"abi":offer["abi"],"version":1,"op":"ready","sequence":1,
        "configuration":offer["configuration"],"instance_epoch":offer["expected"]["instance_epoch"],
        "challenge":offer["challenge"]})
}
fn write_ready(peer: &mut UnixStream, value: &Value) {
    peer.write_all(&serde_json::to_vec(value).unwrap()).unwrap();
    peer.write_all(b"\n").unwrap();
}
fn serve_canonical(listener: UnixListener, responder: Arc<Responder>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let end = Instant::now() + Duration::from_secs(3);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < end => {
                    thread::sleep(Duration::from_millis(2))
                }
                result => panic!("private canonical accept: {result:?}"),
            }
        };
        assert!(desktop::same_user(&stream));
        // BSD accepts may inherit the listener's nonblocking flag.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut query = Vec::new();
        BufReader::new(&mut stream)
            .take(64)
            .read_until(b'\n', &mut query)
            .unwrap();
        let reply = responder.reply(query.strip_suffix(b"\n").unwrap()).unwrap();
        stream.write_all(&reply).unwrap();
    })
}

#[test]
fn registry_validates_its_fixed_configuration_and_service_identity() {
    for (config, service) in [
        ("A".repeat(64), SERVICE),
        ("a".repeat(64), "00000000-0000-0000-0000-000000000000"),
        ("a".repeat(64), "invalid"),
    ] {
        assert!(Registry::new(config, service.into()).is_err());
    }
    let registry = registry();
    let (stream, mut peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let mut bad = expected();
    bad.build.source_dirty = Some(true);
    assert!(registry.begin(BufReader::new(stream), bad).is_err());
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    assert!(!registry.state.lock().unwrap().occupied);
}

#[test]
fn one_slot_is_reserved_before_worker_handoff_and_only_actual_drop_releases_it() {
    let registry = Arc::new(registry());
    let barrier = Arc::new(Barrier::new(3));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let registry = registry.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let (stream, peer) = UnixStream::pair().unwrap();
                barrier.wait();
                (registry.begin(BufReader::new(stream), expected()), peer)
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|(r, _)| r.is_ok()).count(), 1);
    assert!(registry.state.lock().unwrap().occupied);
    drop(results);
    let (pending, mut peer) = pending(&registry);
    drop(pending);
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    assert!(!registry.state.lock().unwrap().occupied);
}

#[test]
fn close_and_poison_are_sticky_without_releasing_an_accepted_worker() {
    let registry = registry();
    let (pending, _peer) = pending(&registry);
    registry.close().unwrap();
    assert!(pending.bind(Path::new("unused")).is_err());
    let (stream, _peer) = UnixStream::pair().unwrap();
    assert!(registry.begin(BufReader::new(stream), expected()).is_err());
    assert!(registry.state.lock().unwrap().closed);
    let registry = Arc::new(Registry::new("a".repeat(64), SERVICE.into()).unwrap());
    let other = registry.clone();
    assert!(thread::spawn(move || {
        let _held = other.state.lock().unwrap();
        panic!("private registry poison");
    })
    .join()
    .is_err());
    assert!(registry.close().is_err());
    let (stream, _peer) = UnixStream::pair().unwrap();
    assert!(registry.begin(BufReader::new(stream), expected()).is_err());
}

#[test]
fn exact_cached_responder_binds_the_original_control_fd_and_retains_its_tail() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let responder = responder();
    let server = serve_canonical(listener, responder.clone());
    let registry = registry();
    let (pending, mut peer) = pending(&registry);
    let client = thread::spawn(move || {
        let offer = offer(&mut peer);
        assert_eq!(offer["expected"], serde_json::to_value(expected()).unwrap());
        let challenge: Challenge = serde_json::from_value(offer["challenge"].clone()).unwrap();
        assert_ne!(challenge.nonce, NONCE);
        assert_ne!(challenge.producer_epoch, INSTANCE);
        assert!(responder.install_challenge(challenge, Instant::now()));
        let mut bytes = serde_json::to_vec(&ready(&offer)).unwrap();
        bytes.extend_from_slice(b"\ntail\n");
        peer.write_all(&bytes).unwrap();
        peer
    });
    let mut session = pending.bind(&path).unwrap();
    server.join().unwrap();
    let mut peer = client.join().unwrap();
    assert_eq!(session.identity().hello().instance_epoch, INSTANCE);
    assert_eq!(session.identity().configuration(), "a".repeat(64));
    assert_eq!(session.identity().challenge().service_epoch, SERVICE);
    assert_eq!(
        session
            ._channel
            .line(Instant::now() + Duration::from_secs(1))
            .unwrap()
            .unwrap(),
        b"tail\n"
    );
    assert!(registry.state.lock().unwrap().occupied);
    drop(session);
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    assert!(!registry.state.lock().unwrap().occupied);
}

#[test]
fn malformed_or_mismatched_readiness_never_probes_or_rebinds() {
    for change in 0..10 {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("s");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let registry = registry();
        let (pending, mut peer) = pending(&registry);
        let client = thread::spawn(move || {
            let offer = offer(&mut peer);
            let mut value = ready(&offer);
            match change {
                0 => value["version"] = json!(2),
                1 => value["op"] = json!("drained"),
                2 => value["sequence"] = json!(0),
                3 => value["configuration"] = json!("c".repeat(64)),
                4 => value["instance_epoch"] = json!(SERVICE),
                5 => value["challenge"]["nonce"] = json!(NONCE),
                6 => value["challenge"]["service_epoch"] = json!(INSTANCE),
                7 => value["challenge"]["producer_epoch"] = json!(SERVICE),
                8 => value["authority"] = json!(true),
                _ => value["challenge"]["extra"] = json!(true),
            }
            write_ready(&mut peer, &value);
        });
        assert!(pending.bind(&path).is_err());
        client.join().unwrap();
        assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
        assert!(!registry.state.lock().unwrap().occupied);
    }
}

#[test]
fn ready_without_the_canonical_cached_offer_never_binds() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let server = serve_canonical(listener, responder());
    let registry = registry();
    let (pending, mut peer) = pending(&registry);
    let client = thread::spawn(move || {
        let offer = offer(&mut peer);
        write_ready(&mut peer, &ready(&offer));
    });
    assert!(pending.bind(&path).is_err());
    client.join().unwrap();
    server.join().unwrap();
    assert!(!registry.state.lock().unwrap().occupied);
}

#[test]
fn closure_while_the_worker_waits_for_ready_prevents_the_canonical_probe() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let registry = registry();
    let (pending, mut peer) = pending(&registry);
    let worker_path = path.clone();
    let worker = thread::spawn(move || pending.bind(&worker_path));
    let offer = offer(&mut peer);
    registry.close().unwrap();
    assert!(registry.state.lock().unwrap().occupied);
    write_ready(&mut peer, &ready(&offer));
    assert!(worker.join().unwrap().is_err());
    assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    assert!(!registry.state.lock().unwrap().occupied);
    assert!(registry.state.lock().unwrap().closed);
}

#[test]
fn expired_partial_duplicate_utf8_and_eof_frames_are_terminal() {
    for bytes in [
        &b"{"[..],
        &b"{}\n"[..],
        &b"{\"abi\":\"a\",\"abi\":\"a\"}\n"[..],
        &b"\xff\n"[..],
        &b""[..],
    ] {
        let registry = registry();
        let (pending, mut peer) = pending(&registry);
        let bytes = bytes.to_vec();
        let client = thread::spawn(move || {
            offer(&mut peer);
            peer.write_all(&bytes).unwrap();
        });
        assert!(pending.bind(Path::new("unused")).is_err());
        client.join().unwrap();
        assert!(!registry.state.lock().unwrap().occupied);
    }
    let registry = registry();
    let (pending, mut peer) = pending(&registry);
    // Exercise the real original ten-second deadline, without resetting it or
    // introducing a production clock override merely for this fixture.
    thread::sleep(
        pending
            .attempt
            .as_ref()
            .unwrap()
            .deadline()
            .saturating_duration_since(Instant::now()),
    );
    assert!(pending.bind(Path::new("unused")).is_err());
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
}

#[test]
fn dropped_outer_receiver_does_not_free_the_workers_pending_connection() {
    let registry = registry();
    let (pending, mut peer) = pending(&registry);
    let (sent, receiver) = mpsc::sync_channel::<Pending>(1);
    let (entered, ready) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    drop(receiver);
    let worker = thread::spawn(move || {
        let retained = sent.send(pending).unwrap_err().0;
        entered.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(retained);
    });
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(registry.state.lock().unwrap().occupied);
    let (stream, _peer) = UnixStream::pair().unwrap();
    assert!(registry.begin(BufReader::new(stream), expected()).is_err());
    registry.close().unwrap();
    assert!(registry.state.lock().unwrap().occupied);
    release.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    assert!(!registry.state.lock().unwrap().occupied);
    assert!(registry.state.lock().unwrap().closed);
}
