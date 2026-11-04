use super::*;
const NONCE: &str = "11111111-1111-4111-8111-111111111111";
const EPOCH: &str = "22222222-2222-4222-8222-222222222222";
fn responder() -> Responder {
    Responder::new(
        "com.hgdev.hgs-tray".into(),
        Build {
            version: "1.2.3-dev.4".into(),
            source_commit: "b".repeat(40),
            source_dirty: Some(false),
        },
        EPOCH.into(),
        true,
        false,
    )
}
fn hello() -> Hello {
    let responder = responder();
    assert!(responder.configure_once("a".repeat(64)));
    serde_json::from_slice(&responder.reply(&query(NONCE).unwrap()[..58]).unwrap()).unwrap()
}
#[test]
fn query_fits_the_original_line_and_has_an_exact_nonce() {
    let query = query(NONCE).unwrap();
    assert_eq!(query.len(), 59);
    assert!(query.len() <= 64);
    assert_eq!(query_nonce(&query[..query.len() - 1]), Some(NONCE));
    for bad in [
        "show",
        "sessions",
        "desktop-delegation-v2 11111111-1111-4111-8111-111111111111",
        "desktop-delegation-v1  11111111-1111-4111-8111-111111111111",
        "desktop-delegation-v1 11111111-1111-4111-8111-111111111111 ",
    ] {
        assert!(query_nonce(bad.as_bytes()).is_none());
    }
}
#[test]
fn identity_requires_exact_configuration_nonce_production_build_and_live_instance() {
    let good = hello();
    assert!(good.identity_matches(NONCE, &"a".repeat(64)));
    assert!(!good.identity_matches(EPOCH, &"a".repeat(64)));
    assert!(!good.identity_matches(NONCE, &"c".repeat(64)));
    let mut bad = good.clone();
    bad.version = 2;
    assert!(!bad.identity_matches(NONCE, &"a".repeat(64)));
    let mut bad = good.clone();
    bad.application = "dev.zerus.next".into();
    assert!(!bad.identity_matches(NONCE, &"a".repeat(64)));
    let mut bad = good.clone();
    bad.replay = true;
    assert!(!bad.identity_matches(NONCE, &"a".repeat(64)));
    let mut bad = good.clone();
    bad.live = false;
    assert!(!bad.identity_matches(NONCE, &"a".repeat(64)));
    let mut bad = good.clone();
    bad.build.source_dirty = None;
    assert!(!bad.identity_matches(NONCE, &"a".repeat(64)));
    let mut bad = good.clone();
    bad.build.source_dirty = Some(true);
    assert!(!bad.identity_matches(NONCE, &"a".repeat(64)));
    let mut bad = good.clone();
    bad.instance_epoch = "stale".into();
    assert!(!bad.identity_matches(NONCE, &"a".repeat(64)));
    let mut bad = serde_json::to_value(good).unwrap();
    bad["unsolicited"] = true.into();
    assert!(serde_json::from_value::<Hello>(bad).is_err());
}
#[test]
fn configuration_is_set_once_and_an_unready_reply_grants_nothing() {
    let responder = responder();
    let request = query(NONCE).unwrap();
    let before: Hello = serde_json::from_slice(&responder.reply(&request[..58]).unwrap()).unwrap();
    assert!(!before.identity_matches(NONCE, &"a".repeat(64)));
    assert!(!responder.configure_once("invalid".into()));
    assert!(responder.configure_once("a".repeat(64)));
    assert!(!responder.configure_once("c".repeat(64)));
    let after: Hello = serde_json::from_slice(&responder.reply(&request[..58]).unwrap()).unwrap();
    assert!(after.identity_matches(NONCE, &"a".repeat(64)));
    assert_eq!(after.writer_epoch, None);
    assert!(responder.reply(b"show").is_none());
    assert!(responder.reply(b"sessions").is_none());
}

#[cfg(unix)]
mod sockets {
    use super::*;
    use std::{
        fs,
        io::{Read, Write},
        os::unix::net::UnixListener,
        path::PathBuf,
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::Instant,
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "zdh-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn peer(reply: Vec<u8>, silent: bool, slow: bool) -> Observation {
        measured_peer(reply, silent, slow, Duration::ZERO).0
    }
    fn measured_peer(
        reply: Vec<u8>,
        silent: bool,
        slow: bool,
        cleanup_delay: Duration,
    ) -> (Observation, Duration) {
        const FIXTURE_WAIT: Duration = Duration::from_secs(10);
        let root = Scratch::new();
        let path = root.0.join("s");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (done, completed) = mpsc::sync_channel(1);
        let task = thread::spawn(move || {
            let until = Instant::now() + FIXTURE_WAIT;
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < until, "fixture was not connected");
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            stream.set_read_timeout(Some(FIXTURE_WAIT)).unwrap();
            stream.set_write_timeout(Some(FIXTURE_WAIT)).unwrap();
            assert!(same_user(&stream));
            let mut bytes = Vec::new();
            let mut b = [0];
            while stream.read(&mut b).unwrap_or(0) == 1 {
                bytes.push(b[0]);
                if b[0] == b'\n' {
                    break;
                }
            }
            assert_eq!(bytes, query(NONCE).unwrap());
            if silent {
                thread::sleep(Duration::from_millis(1100));
            } else if slow {
                for b in reply {
                    if stream.write_all(&[b]).is_err() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(30));
                }
            } else {
                let _ = stream.write_all(&reply);
            }
            // Fixture scheduling/cleanup is independent of the product probe.
            thread::sleep(cleanup_delay);
            done.send(()).unwrap();
        });
        let start = Instant::now();
        let result = probe_at(&path, NONCE, &"a".repeat(64));
        let elapsed = start.elapsed();
        completed
            .recv_timeout(FIXTURE_WAIT)
            .expect("socket fixture did not finish");
        task.join().unwrap();
        (result, elapsed)
    }

    #[test]
    fn cached_producer_offer_is_one_use_over_the_authenticated_socket() {
        use super::super::producer::{Challenge, Observation as Producer};
        let root = Scratch::new();
        let path = root.0.join("p");
        let listener = UnixListener::bind(&path).unwrap();
        let responder = responder();
        responder.configure_once("a".repeat(64));
        let expected = responder.hello(NONCE);
        let offer = Challenge {
            nonce: NONCE.into(),
            service_epoch: EPOCH.into(),
            producer_epoch: "33333333-3333-4333-8333-333333333333".into(),
        };
        let deadline = Instant::now() + producer::CHALLENGE_WAIT;
        assert!(responder.install_challenge(offer.clone(), Instant::now()));
        let task = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                assert!(same_user(&stream));
                stream.set_read_timeout(Some(PROBE_WAIT)).unwrap();
                let mut line = Vec::new();
                let mut byte = [0];
                while stream.read(&mut byte).unwrap() == 1 && byte[0] != b'\n' {
                    line.push(byte[0]);
                    assert!(line.len() < 64);
                }
                stream.write_all(&responder.reply(&line).unwrap()).unwrap();
            }
        });
        let Producer::LiveNext { reply } = probe_producer_at(&path, NONCE, &"a".repeat(64)) else {
            panic!("missing exact reply")
        };
        assert!(reply.binds(&offer, &expected, deadline));
        let Producer::LiveNext { reply } = probe_producer_at(&path, NONCE, &"a".repeat(64)) else {
            panic!("missing diagnostic reply")
        };
        assert!(!reply.binds(&offer, &expected, deadline));
        assert!(reply.challenge.is_none());
        task.join().unwrap();
    }
    #[test]
    fn valid_same_user_challenge_is_identity_evidence_only() {
        let mut bytes = serde_json::to_vec(&hello()).unwrap();
        bytes.push(b'\n');
        assert!(matches!(
            peer(bytes, false, false),
            Observation::LiveNext { .. }
        ));
    }
    #[test]
    fn ignored_or_rejected_queries_are_live_unknown_never_absent() {
        assert_eq!(
            peer(Vec::new(), false, false),
            Observation::LiveLegacyOrUnknown
        );
        assert_eq!(
            peer(Vec::new(), true, false),
            Observation::LiveLegacyOrUnknown
        );
    }
    #[test]
    fn stale_unsolicited_newer_and_oversized_replies_are_passive() {
        let mut wrong = hello();
        wrong.nonce = EPOCH.into();
        let mut bytes = serde_json::to_vec(&wrong).unwrap();
        bytes.push(b'\n');
        assert_eq!(peer(bytes, false, false), Observation::LiveLegacyOrUnknown);
        assert_eq!(
            peer(vec![b'x'; REPLY_BYTES + 1], false, false),
            Observation::LiveLegacyOrUnknown
        );
        assert_eq!(
            peer(b"{}\n{}\n".to_vec(), false, false),
            Observation::LiveLegacyOrUnknown
        );
    }
    #[test]
    fn total_deadline_is_not_extended_by_slow_reply_bytes() {
        // This exceeds the elapsed assertion by itself, reproducing a delayed
        // fixture exit after a correct probe. Only probe_at is under test.
        let (result, elapsed) =
            measured_peer(vec![b'x'; 64], false, true, Duration::from_millis(1800));
        assert_eq!(result, Observation::LiveLegacyOrUnknown);
        assert!(
            elapsed < Duration::from_millis(1600),
            "probe took {elapsed:?}"
        );
    }
    #[test]
    fn only_a_missing_path_is_absent_and_existing_entries_are_untouched() {
        let root = Scratch::new();
        let path = root.0.join("s");
        assert_eq!(probe_at(&path, NONCE, &"a".repeat(64)), Observation::Absent);
        fs::write(&path, b"existing").unwrap();
        assert_eq!(
            probe_at(&path, NONCE, &"a".repeat(64)),
            Observation::Unknown
        );
        assert_eq!(fs::read(&path).unwrap(), b"existing");
        fs::remove_file(&path).unwrap();
        drop(UnixListener::bind(&path).unwrap());
        // The closed endpoint can fail at connect or at the subsequent exchange
        // (observed on XNU under the parallel harness). Both remain passive:
        // neither absence nor an authenticated live Next identity is acceptable.
        let observed = probe_at(&path, NONCE, &"a".repeat(64));
        assert!(
            matches!(
                observed,
                Observation::Unknown | Observation::LiveLegacyOrUnknown
            ),
            "{observed:?}"
        );
        assert!(path.exists());
    }
    #[test]
    fn a_full_backlog_never_turns_connect_into_an_unbounded_wait() {
        use socket2::{Domain, SockAddr, Socket, Type};
        let root = Scratch::new();
        let path = root.0.join("s");
        let server = Socket::new(Domain::UNIX, Type::STREAM, None).unwrap();
        server.bind(&SockAddr::unix(&path).unwrap()).unwrap();
        server.listen(1).unwrap();
        let mut clients = Vec::new();
        for _ in 0..4 {
            let client = Socket::new(Domain::UNIX, Type::STREAM, None).unwrap();
            client.set_nonblocking(true).unwrap();
            let _ = client.connect(&SockAddr::unix(&path).unwrap());
            clients.push(client);
        }
        let start = Instant::now();
        assert!(matches!(
            probe_at(&path, NONCE, &"a".repeat(64)),
            Observation::Unknown | Observation::LiveLegacyOrUnknown
        ));
        assert!(start.elapsed() < Duration::from_millis(1600));
    }

    #[test]
    fn selected_cli_is_probed_safely_before_a_desktop_command() {
        let root = Scratch::new();
        let hgs = root.0.join("hgs");
        std::os::unix::fs::symlink(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_hgs_import.py"),
            &hgs,
        )
        .unwrap();
        let env = [("TEST_IMPORT_ROOT", root.0.to_str().unwrap())];
        let options = root.0.join("options.json");
        let log = root.0.join("calls.jsonl");
        fs::write(
            &options,
            r#"{"help":"Usage: hgs\nDesktop service ABI: desktop-v1-suffix"}"#,
        )
        .unwrap();
        assert_eq!(selected_configuration(&hgs, &env), None);
        assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 1);
        fs::write(
            &options,
            r#"{"help":"Usage: hgs\n Desktop service ABI: desktop-v1"}"#,
        )
        .unwrap();
        assert_eq!(selected_configuration(&hgs, &env), Some("a".repeat(64)));
        assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 3);
        fs::write(
            &options,
            r#"{"help":"Desktop service ABI: desktop-v1","configuration":"wrong"}"#,
        )
        .unwrap();
        assert_eq!(selected_configuration(&hgs, &env), None);
    }
}
