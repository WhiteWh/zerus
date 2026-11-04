use super::*;
use crate::desktop::store::Store;
use serde_json::json;
use std::os::unix::fs::MetadataExt;
use std::{io::BufRead, path::PathBuf, sync::atomic::AtomicUsize};
use zerus_desktop_ledger as ledger;

const CHILD: &str = "desktop::notifications::host::tests::child_fixture";
const CHATTY: &str = "desktop::notifications::host::tests::child_chatty_fixture";
static DRIVER_TEST: Mutex<()> = Mutex::new(());
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
// Data-only generation adapter for controller tests, not signature evidence.
// PR930 separately tests the actual filesystem generation comparison.
struct GenerationFixture {
    ancestor: PathBuf,
    helper: PathBuf,
    identity: (u64, u64, u32, u32),
    helper_stamp: ((u64, u64, u32, u32), u64, std::time::SystemTime),
}
impl GenerationFixture {
    fn identity(meta: &std::fs::Metadata) -> (u64, u64, u32, u32) {
        (meta.dev(), meta.ino(), meta.mode(), meta.uid())
    }
    fn new(root: &Path) -> Arc<Self> {
        let helper = root.join("helper-marker");
        std::fs::write(&helper, b"original").unwrap();
        let meta = helper.metadata().unwrap();
        Arc::new(Self {
            ancestor: root.into(),
            helper,
            identity: Self::identity(&root.metadata().unwrap()),
            helper_stamp: (Self::identity(&meta), meta.len(), meta.modified().unwrap()),
        })
    }
    fn current(&self) -> bool {
        let Ok(meta) = self.helper.metadata() else {
            return false;
        };
        self.ancestor
            .metadata()
            .is_ok_and(|m| Self::identity(&m) == self.identity)
            && (Self::identity(&meta), meta.len(), meta.modified().unwrap()) == self.helper_stamp
    }
}
struct FixtureNative {
    image: PathBuf,
    checks: Arc<AtomicUsize>,
    generation: Option<Arc<GenerationFixture>>,
}
impl Native for FixtureNative {
    fn executable(&self) -> &Path {
        &self.image
    }
    fn revalidate(&self, child: Option<&mut Child>) -> Result<()> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        ensure!(
            self.generation.as_ref().is_none_or(|g| g.current()),
            "fixture helper generation changed"
        );
        if let Some(child) = child {
            ensure!(child.try_wait()?.is_none(), "fixture child exited");
        }
        Ok(())
    }
}
fn launch(
    location: &Location,
    session: wire::Session,
    endpoint: Endpoint,
    checks: Arc<AtomicUsize>,
    chatty: bool,
    generation: Option<Arc<GenerationFixture>>,
) -> OwnedHost {
    OwnedHost::start(
        FixtureNative {
            image: std::env::current_exe().unwrap(),
            checks,
            generation,
        },
        location,
        session,
        endpoint,
        vec![
            "--exact".into(),
            if chatty { CHATTY } else { CHILD }.into(),
            "--nocapture".into(),
        ],
    )
    .unwrap()
}
fn wait(mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(8);
    while !done() {
        assert!(Instant::now() < until, "fixture deadline");
        thread::sleep(Duration::from_millis(2));
    }
}

/// A real private OS child, but a synthetic verifier/protocol peer. It proves
/// fd/lock/EOF ownership and resource retention, never native Mac code identity.
#[test]
fn child_fixture() {
    run_child(CHILD, false);
}
#[test]
fn child_chatty_fixture() {
    run_child(CHATTY, true);
}
fn run_child(selected: &str, chatty: bool) {
    if !std::env::args().any(|arg| arg == "--exact") || !std::env::args().any(|arg| arg == selected)
    {
        return;
    }
    let lock = unsafe { File::from_raw_fd(3) };
    assert!(platform::verify_private_file(&lock).unwrap());
    let mut stream = unsafe { UnixStream::from_raw_fd(0) };
    let mut reader = io::BufReader::new(stream.try_clone().unwrap());
    let mut line = Vec::new();
    reader
        .by_ref()
        .take(wire::FRAME_BYTES as u64)
        .read_until(b'\n', &mut line)
        .unwrap();
    let wire::ToHost::Hello { session, abi } = wire::decode(&line).unwrap() else {
        panic!("hello required");
    };
    assert_eq!(abi, wire::ABI);
    if chatty {
        std::io::stderr()
            .write_all(&vec![b'x'; LOG_BYTES + QUANTUM])
            .unwrap();
    }
    stream
        .write_all(
            &wire::encode(&wire::FromHost::Ready {
                abi,
                application: wire::APPLICATION.into(),
                session: session.clone(),
            })
            .unwrap(),
        )
        .unwrap();
    loop {
        line.clear();
        reader
            .by_ref()
            .take(wire::FRAME_BYTES as u64)
            .read_until(b'\n', &mut line)
            .unwrap();
        if line.is_empty() {
            break;
        }
        match wire::decode::<wire::ToHost>(&line).unwrap() {
            wire::ToHost::Command(command) => {
                let wire::Action::Post { token, body, .. } = &command.action else {
                    panic!("fixture post");
                };
                let target = zerus_client_core::attention::target(token).unwrap();
                let fd = target["foreign_fd"].as_i64().unwrap() as i32;
                let mut stat: libc::stat = unsafe { std::mem::zeroed() };
                let found = unsafe { libc::fstat(fd, &mut stat) } == 0;
                assert!(
                    !found
                        || stat.st_dev as u64 != target["foreign_dev"].as_u64().unwrap()
                        || stat.st_ino as u64 != target["foreign_ino"].as_u64().unwrap(),
                    "foreign non-CLOEXEC file leaked into host"
                );
                let native_id = wire::NativeId::Macos {
                    request_id: zerus_client_core::attention::request_id(token),
                };
                stream
                    .write_all(
                        &wire::encode(&wire::FromHost::Receipt(wire::Receipt {
                            binding: command.binding.clone(),
                            outcome: wire::Outcome::Delivered { native_id },
                        }))
                        .unwrap(),
                    )
                    .unwrap();
                let gate = PathBuf::from(body);
                wait(|| gate.is_file());
                stream
                    .write_all(
                        &wire::encode(&wire::FromHost::Released {
                            binding: command.binding,
                        })
                        .unwrap(),
                    )
                    .unwrap();
            }
            wire::ToHost::Stop { session: observed } => {
                assert_eq!(observed, session);
                stream
                    .write_all(&wire::encode(&wire::FromHost::Stopped { session }).unwrap())
                    .unwrap();
                break;
            }
            wire::ToHost::Restore { .. } => (),
            _ => panic!("fixture protocol"),
        }
    }
}

#[test]
fn original_child_keeps_owner_lock_and_receipt_precedes_actual_release() {
    exercise_driver(false, false);
}
#[test]
fn stderr_prefix_overflow_does_not_disable_native_commands() {
    exercise_driver(true, false);
}
#[test]
fn ancestor_churn_between_commands_dispatches_but_changed_helper_refuses() {
    exercise_driver(false, true);
}
fn exercise_driver(chatty: bool, churn: bool) {
    let _serial = DRIVER_TEST.lock().unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let location = Location {
        root: temp.path().canonicalize().unwrap().join("state/desktop"),
        configuration: "a".repeat(64),
    };
    location.prepare().unwrap();
    let session = wire::Session {
        configuration: location.configuration.clone(),
        service_epoch: id(),
        writer_epoch: id(),
        adapter: id(),
        host_epoch: id(),
        lease: id(),
        platform: wire::Platform::Macos,
    };
    let mut store = Store::open(&location, &session.service_epoch).unwrap();
    ledger::authorize(&mut store.db, &session.writer_epoch, &session.service_epoch).unwrap();
    let (mut outbox, endpoint) = super::super::Outbox::new(session.clone()).unwrap();
    let checks = Arc::new(AtomicUsize::new(0));
    let foreign = tempfile::tempfile().unwrap();
    let meta = foreign.metadata().unwrap();
    let raw = unsafe { libc::fcntl(foreign.as_raw_fd(), libc::F_DUPFD, 200) };
    assert!(raw >= 200);
    let inherited_foreign = unsafe { OwnedFd::from_raw_fd(raw) };
    let generation = churn.then(|| GenerationFixture::new(temp.path()));
    let mut host = launch(
        &location,
        session.clone(),
        endpoint,
        checks.clone(),
        chatty,
        generation.clone(),
    );
    wait(|| host.status()["transport_ready"] == true);
    assert_eq!(host.status()["ready"], false);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(location.root.join("notification-host.lock"))
        .unwrap();
    assert!(fs2::FileExt::try_lock_exclusive(&file).is_err());
    let (_, second_endpoint) = super::super::Outbox::new(session.clone()).unwrap();
    assert!(OwnedHost::start(
        FixtureNative {
            image: std::env::current_exe().unwrap(),
            checks: checks.clone(),
            generation: None,
        },
        &location,
        session.clone(),
        second_endpoint,
        vec![]
    )
    .is_err());
    let gate = temp.path().join("release");
    let request = offer_fixture(&mut store, &mut outbox, &gate, &inherited_foreign, &meta);
    let turn = host.status()["turns"].as_u64().unwrap_or(0);
    wait(|| host.status()["turns"].as_u64().unwrap_or(0) >= turn + 2);
    assert_eq!(
        checks.load(Ordering::SeqCst),
        2,
        "new native work waits for restoration"
    );
    assert!(outbox.pending[0].shared.reply.lock().unwrap().is_none());
    host.finish_restoration();
    wait(|| host.status()["ready"] == true);
    wait(|| {
        outbox.poll(&mut store, Instant::now());
        outbox
            .pending
            .first()
            .is_some_and(|p| p.shared.reply.lock().unwrap().is_some())
    });
    let state = |store: &Store| -> String {
        store
            .db
            .query_row(
                "SELECT state FROM desktop_attempts WHERE request_id=?1",
                [&request.request_id],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(
        state(&store),
        "delivered",
        "the exact receipt settles before native resource release"
    );
    assert!(
        !outbox.operations_idle(),
        "the original child still holds resources"
    );
    std::fs::write(&gate, b"release").unwrap();
    wait(|| {
        outbox.poll(&mut store, Instant::now());
        outbox.operations_idle()
    });
    assert!(
        checks.load(Ordering::SeqCst) >= 3,
        "selection/child are rechecked before dispatch"
    );
    if chatty {
        let status = host.status();
        assert_eq!(status["stderr_bytes"], LOG_BYTES);
        assert_eq!(status["log_overflow"], true);
        assert!(status["log_dropped_bytes"].as_u64().unwrap() >= QUANTUM as u64);
        assert!(status["fault"].is_null(), "{status}");
    }
    if let Some(generation) = generation {
        std::fs::write(
            temp.path().join("ancestor-history"),
            b"unrelated ancestor update",
        )
        .unwrap();
        let second = offer_fixture(&mut store, &mut outbox, &gate, &inherited_foreign, &meta);
        wait(|| {
            outbox.poll(&mut store, Instant::now());
            store
                .db
                .query_row(
                    "SELECT state FROM desktop_attempts WHERE request_id=?1",
                    [&second.request_id],
                    |r| r.get::<_, String>(0),
                )
                .unwrap()
                == "delivered"
        });
        std::fs::write(&generation.helper, b"changed helper generation").unwrap();
        let third = offer_fixture(&mut store, &mut outbox, &gate, &inherited_foreign, &meta);
        wait(|| {
            outbox.poll(&mut store, Instant::now());
            store
                .db
                .query_row(
                    "SELECT state FROM desktop_attempts WHERE request_id=?1",
                    [&third.request_id],
                    |r| r.get::<_, String>(0),
                )
                .unwrap()
                == "refused"
        });
    }
    host.request_stop();
    wait(|| host.poll_drained());
    assert_eq!(host.status()["exit"], 0);
    if churn {
        assert_eq!(host.status()["fault"], "notification_pair_changed");
    } else {
        assert!(host.status()["fault"].is_null(), "{}", host.status());
    }
    // The host is joined, but a concurrent fork can retain a duplicate of the
    // shared open-file description until exec closes its CLOEXEC descriptors.
    // Require actual lock release within the fixture deadline, not this turn.
    wait(|| fs2::FileExt::try_lock_exclusive(&file).is_ok());
}

fn offer_fixture(
    store: &mut Store,
    outbox: &mut super::super::Outbox,
    gate: &Path,
    inherited_foreign: &OwnedFd,
    meta: &std::fs::Metadata,
) -> ledger::Reserve {
    let token = zerus_client_core::attention::encode_token(json!({"host":"peer.example","name":"fixture","run":"fixture-run","conversation":"fixture-conversation","notice":id(),"foreign_fd":inherited_foreign.as_raw_fd(),"foreign_dev":meta.dev(),"foreign_ino":meta.ino()}).as_object().unwrap());
    let intent = ledger::put_intent(
        &mut store.db,
        &ledger::Identity {
            kind: ledger::Kind::Post,
            target: ledger::Target::Session {
                computer: "peer.example".into(),
                run: "fixture-run".into(),
                conversation: "fixture-conversation".into(),
            },
            edge: id(),
        },
        1,
        &serde_json::to_value(wire::Action::Post {
            token,
            title: "Fixture".into(),
            body: gate.to_str().unwrap().into(),
        })
        .unwrap(),
    )
    .unwrap();
    let session = outbox.session.clone();
    let request = ledger::Reserve {
        request_id: id(),
        intent_id: intent,
        revision: 1,
        epoch: session.writer_epoch,
        service_epoch: session.service_epoch,
        adapter: session.adapter,
        lease: session.lease,
        retry_of: None,
    };
    outbox
        .offer(store, request.clone(), Instant::now())
        .unwrap();
    request
}

#[test]
fn framing_retains_coalesced_messages_and_refuses_a_partial_eof() {
    let (mut read, mut write) = UnixStream::pair().unwrap();
    read.set_nonblocking(true).unwrap();
    let frame = wire::FromHost::Stopped {
        session: wire::Session {
            configuration: "a".repeat(64),
            service_epoch: id(),
            writer_epoch: id(),
            adapter: id(),
            host_epoch: id(),
            lease: id(),
            platform: wire::Platform::Macos,
        },
    };
    let bytes = wire::encode(&frame).unwrap();
    write.write_all(&bytes).unwrap();
    write.write_all(&bytes).unwrap();
    let mut input = Vec::new();
    let mut eof = false;
    assert!(read_frame(&mut read, &mut input, &mut eof)
        .unwrap()
        .is_none());
    assert_eq!(
        read_frame(&mut read, &mut input, &mut eof).unwrap(),
        Some(frame.clone())
    );
    assert_eq!(
        read_frame(&mut read, &mut input, &mut eof).unwrap(),
        Some(frame)
    );
    write.write_all(b"{").unwrap();
    drop(write);
    // Nonblocking reads may observe the byte, WouldBlock/Interrupted and EOF
    // on different turns. Assert the partial-frame error at actual EOF.
    wait(|| {
        let result = read_frame(&mut read, &mut input, &mut eof);
        if eof {
            assert!(result.is_err());
            true
        } else {
            assert!(result.unwrap().is_none());
            false
        }
    });
}

#[test]
fn logs_keep_a_bounded_prefix_and_still_observe_original_eof() {
    let (mut read, mut write) = UnixStream::pair().unwrap();
    read.set_nonblocking(true).unwrap();
    let mut prefix = vec![b'a'; LOG_BYTES - 1];
    let mut eof = false;
    write.write_all(b"xyz").unwrap();
    drop(write);
    let mut dropped = 0;
    wait(|| {
        dropped += log_turn(&mut read, &mut prefix, &mut eof).unwrap();
        eof
    });
    assert_eq!(dropped, 2);
    assert_eq!(prefix.len(), LOG_BYTES);
    assert_eq!(prefix[LOG_BYTES - 1], b'x');
    assert!(eof);
}

fn empty_host_fixture() -> (
    tempfile::TempDir,
    Location,
    wire::Session,
    super::super::Outbox,
    Endpoint,
) {
    let temp = tempfile::tempdir().unwrap();
    let location = Location {
        root: temp.path().canonicalize().unwrap().join("state/desktop"),
        configuration: "a".repeat(64),
    };
    location.prepare().unwrap();
    let session = wire::Session {
        configuration: location.configuration.clone(),
        service_epoch: id(),
        writer_epoch: id(),
        adapter: id(),
        host_epoch: id(),
        lease: id(),
        platform: wire::Platform::Macos,
    };
    let (outbox, endpoint) = super::super::Outbox::new(session.clone()).unwrap();
    (temp, location, session, outbox, endpoint)
}

#[test]
fn restore_burst_drains_without_an_idle_wait_per_mapping() {
    let _serial = DRIVER_TEST.lock().unwrap_or_else(|e| e.into_inner());
    let (_temp, location, session, _outbox, endpoint) = empty_host_fixture();
    let mut host = launch(
        &location,
        session,
        endpoint,
        Arc::new(AtomicUsize::new(0)),
        false,
        None,
    );
    wait(|| host.status()["transport_ready"] == true);
    // Exclude child startup and descriptor preparation. Restore intentionally
    // receives no acknowledgement from the real private fixture child.
    let start = Instant::now();
    for _ in 0..30 {
        let token = zerus_client_core::attention::encode_token(
            json!({"host":"peer.example","name":"fixture","run":"fixture-run","conversation":"fixture-conversation","notice":id()})
                .as_object()
                .unwrap(),
        );
        let mapping = wire::Activation {
            request_id: id(),
            native_id: wire::NativeId::Macos {
                request_id: zerus_client_core::attention::request_id(&token),
            },
            token,
            platform_context: None,
        };
        wait(|| host.restore(mapping.clone()));
    }
    host.finish_restoration();
    wait(|| host.status()["ready"] == true);
    let elapsed = start.elapsed();
    host.request_stop();
    wait(|| host.poll_drained());
    assert!(host.status()["fault"].is_null(), "{}", host.status());
    assert!(
        elapsed < Duration::from_secs(1),
        "restore burst: {elapsed:?}"
    );
}

#[test]
fn ready_racing_an_immediate_stop_does_not_reopen_or_fault_the_host() {
    let _serial = DRIVER_TEST.lock().unwrap_or_else(|e| e.into_inner());
    let (_temp, location, session, _outbox, endpoint) = empty_host_fixture();
    let mut host = launch(
        &location,
        session,
        endpoint,
        Arc::new(AtomicUsize::new(0)),
        false,
        None,
    );
    host.request_stop();
    wait(|| host.poll_drained());
    assert_eq!(host.status()["ready"], false);
    assert!(host.status()["fault"].is_null(), "{}", host.status());
}

#[test]
fn foreign_fd_cleanup_preserves_the_exec_error_pipe() {
    let _serial = DRIVER_TEST.lock().unwrap_or_else(|e| e.into_inner());
    let (temp, location, session, _outbox, endpoint) = empty_host_fixture();
    let mut host = OwnedHost::start(
        FixtureNative {
            image: temp.path().join("missing-image"),
            checks: Arc::new(AtomicUsize::new(0)),
            generation: None,
        },
        &location,
        session,
        endpoint,
        Vec::new(),
    )
    .unwrap();
    wait(|| host.poll_drained());
    assert_eq!(host.status()["fault"], "notification_host_start_refused");
    assert_eq!(
        host.status()["exit_observed"],
        false,
        "spawn must return its actual exec error"
    );
}
