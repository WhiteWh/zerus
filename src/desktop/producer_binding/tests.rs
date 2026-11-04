use super::*;
use std::time::Duration;
use zerus_client_core::desktop::{self, producer::Reply, Build, Responder};

const EPOCH: &str = "22222222-2222-4222-8222-222222222222";
const NONCE: &str = "11111111-1111-4111-8111-111111111111";
const INSTANCE: &str = "33333333-3333-4333-8333-333333333333";
fn responder() -> Responder {
    let responder = Responder::new(
        "com.hgdev.hgs-tray".into(),
        Build {
            version: "1.2.3-dev.4".into(),
            source_commit: "b".repeat(40),
            source_dirty: Some(false),
        },
        INSTANCE.into(),
        true,
        false,
    );
    assert!(responder.configure_once("a".repeat(64)));
    responder
}
fn expected() -> Hello {
    let query = desktop::query(NONCE).unwrap();
    serde_json::from_slice(&responder().reply(&query[..query.len() - 1]).unwrap()).unwrap()
}
fn attempt() -> Attempt {
    Attempt::new(&"a".repeat(64), EPOCH, expected()).unwrap()
}
fn reply(attempt: &Attempt) -> Reply {
    let mut hello = expected();
    hello.nonce.clone_from(&attempt.challenge.nonce);
    Reply {
        abi: desktop::producer::ABI.into(),
        version: 1,
        hello,
        challenge: Some(attempt.challenge.clone()),
        read_delivery: None,
    }
}
fn observed(reply: Reply) -> Observation {
    Observation::LiveNext {
        reply: Box::new(reply),
    }
}

#[test]
fn attempt_requires_exact_unowned_live_clean_frontend_and_canonical_service_epoch() {
    for epoch in ["", "old", "00000000-0000-0000-0000-000000000000"] {
        assert!(matches!(
            Attempt::new(&"a".repeat(64), epoch, expected()),
            Err(Refused::InvalidIdentity)
        ));
    }
    for change in 0..9 {
        let mut hello = expected();
        match change {
            0 => hello.build.source_dirty = Some(true),
            1 => hello.live = false,
            2 => hello.replay = true,
            3 => hello.writer_epoch = Some(EPOCH.into()),
            4 => hello.configuration = Some("c".repeat(64)),
            5 => hello.instance_epoch = "old".into(),
            6 => hello.application = "dev.example".into(),
            7 => hello.instance_epoch = "00000000-0000-0000-0000-000000000000".into(),
            _ => hello.nonce = "00000000-0000-0000-0000-000000000000".into(),
        }
        assert!(matches!(
            Attempt::new(&"a".repeat(64), EPOCH, hello),
            Err(Refused::InvalidIdentity)
        ));
    }
}

#[test]
fn fresh_host_challenge_and_deadline_are_retained_exactly_without_role_authority() {
    let a = attempt();
    let b = attempt();
    assert!(a.challenge().valid());
    assert_ne!(a.challenge.nonce, b.challenge.nonce);
    assert_ne!(a.challenge.producer_epoch, b.challenge.producer_epoch);
    assert_ne!(a.challenge.nonce, NONCE);
    assert_eq!(a.challenge.service_epoch, EPOCH);
    assert_eq!(a.deadline, a.started + CHALLENGE_WAIT);
    let data = reply(&a);
    let challenge = a.challenge.clone();
    let result = a.complete(observed(data), Instant::now()).unwrap();
    assert_eq!(result.configuration(), "a".repeat(64));
    assert_eq!(result.challenge(), &challenge);
    assert_eq!(result.hello().instance_epoch, INSTANCE);
    assert_eq!(result.hello().nonce, challenge.nonce);
    assert_eq!(format!("{result:?}"), "MatchedProducerIdentity { .. }");
}

#[test]
fn changed_challenge_or_frontend_never_matches_the_retained_original() {
    for change in 0..7 {
        let a = attempt();
        let mut data = reply(&a);
        match change {
            0 => data.challenge = None,
            1 => data.challenge.as_mut().unwrap().service_epoch = INSTANCE.into(),
            2 => data.challenge.as_mut().unwrap().producer_epoch = EPOCH.into(),
            3 => data.hello.instance_epoch = EPOCH.into(),
            4 => data.hello.build.source_commit = "c".repeat(40),
            5 => data.hello.writer_epoch = Some(EPOCH.into()),
            _ => data.hello.configuration = Some("c".repeat(64)),
        }
        assert!(matches!(
            a.complete(observed(data), Instant::now()),
            Err(Refused::Mismatch)
        ));
    }
    let a = attempt();
    let original = reply(&a);
    let b = attempt();
    assert!(matches!(
        b.complete(observed(original), Instant::now()),
        Err(Refused::Mismatch)
    ));
}

#[test]
fn fixed_expiry_and_negative_outcomes_cannot_turn_into_absence_or_match() {
    for delta in [Duration::ZERO, Duration::from_nanos(1)] {
        let a = attempt();
        let now = a.deadline + delta;
        let data = reply(&a);
        assert!(matches!(
            a.complete(observed(data), now),
            Err(Refused::Expired)
        ));
    }
    for (observation, refused) in [
        (Observation::Absent, Refused::Absent),
        (Observation::Unknown, Refused::Unknown),
        (
            Observation::LiveLegacyOrUnknown,
            Refused::LiveLegacyOrUnknown,
        ),
    ] {
        let a = attempt();
        assert!(matches!(a.complete(observation, Instant::now()), Err(r) if r==refused));
    }
    let a = attempt();
    let before = a.started - Duration::from_nanos(1);
    assert!(matches!(
        a.complete(Observation::Absent, before),
        Err(Refused::Unavailable)
    ));
}

#[cfg(unix)]
mod sockets {
    use super::*;
    use std::{
        io::{Read, Write},
        os::unix::{fs::MetadataExt, net::UnixListener},
        thread,
    };

    #[test]
    fn real_same_user_cached_offer_matches_once_and_an_old_offer_does_not_rebind() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("s");
        let listener = UnixListener::bind(&path).unwrap();
        let inode = std::fs::symlink_metadata(&path).unwrap().ino();
        let responder = responder();
        let a = attempt();
        assert!(responder.install_challenge(a.challenge.clone(), Instant::now()));
        let task = thread::spawn(move || {
            let mut calls = 0;
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                assert!(desktop::same_user(&stream));
                stream.set_read_timeout(Some(desktop::PROBE_WAIT)).unwrap();
                let mut line = Vec::new();
                let mut byte = [0];
                while stream.read(&mut byte).unwrap() == 1 && byte[0] != b'\n' {
                    line.push(byte[0]);
                    assert!(line.len() < 64);
                }
                assert!(desktop::producer::query_nonce(&line).is_some());
                stream.write_all(&responder.reply(&line).unwrap()).unwrap();
                calls += 1;
            }
            calls
        });
        let result = a.probe_at(&path).unwrap();
        assert_eq!(result.hello().instance_epoch, INSTANCE);
        assert!(matches!(attempt().probe_at(&path), Err(Refused::Mismatch)));
        assert_eq!(task.join().unwrap(), 2);
        assert_eq!(std::fs::symlink_metadata(&path).unwrap().ino(), inode);
    }

    #[test]
    fn absent_unsafe_and_connected_legacy_paths_stay_distinct_and_untouched() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("s");
        assert!(matches!(attempt().probe_at(&path), Err(Refused::Absent)));
        std::fs::write(&path, b"preserved").unwrap();
        assert!(matches!(attempt().probe_at(&path), Err(Refused::Unknown)));
        assert_eq!(std::fs::read(&path).unwrap(), b"preserved");
        std::fs::remove_file(&path).unwrap();
        let listener = UnixListener::bind(&path).unwrap();
        let task = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(desktop::PROBE_WAIT)).unwrap();
            let mut byte = [0];
            while stream.read(&mut byte).unwrap() == 1 && byte[0] != b'\n' {}
            stream.write_all(b"{}\n").unwrap();
        });
        assert!(matches!(
            attempt().probe_at(&path),
            Err(Refused::LiveLegacyOrUnknown)
        ));
        task.join().unwrap();
    }

    #[test]
    fn expired_attempt_refuses_before_any_socket_exchange() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("s");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let expired = Attempt::at(
            &"a".repeat(64),
            EPOCH,
            expected(),
            Instant::now() - CHALLENGE_WAIT,
        )
        .unwrap();
        assert!(matches!(expired.probe_at(&path), Err(Refused::Expired)));
        assert!(matches!(listener.accept(), Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
    }
}
