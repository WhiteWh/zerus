use super::*;
use std::{
    fs::{self, DirBuilder, File},
    io::{Read, Write},
    os::{fd::FromRawFd, unix::fs::DirBuilderExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex, MutexGuard, PoisonError,
    },
    time::{Duration, Instant},
};

static SERIAL: Mutex<()> = Mutex::new(());
static RESOURCES: AtomicUsize = AtomicUsize::new(0);
static NEXT: AtomicUsize = AtomicUsize::new(0);

pub(super) struct Resource;
impl Resource {
    pub(super) fn new() -> Self {
        RESOURCES.fetch_add(1, Ordering::SeqCst);
        Self
    }
}
impl Drop for Resource {
    fn drop(&mut self) {
        assert!(RESOURCES.fetch_sub(1, Ordering::SeqCst) > 0);
    }
}

fn serial() -> MutexGuard<'static, ()> {
    let guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    assert!(
        std::env::var_os("ZERUS_MAC_PEER_FIXTURES").is_some(),
        "run the private qualification harness"
    );
    assert_eq!(RESOURCES.load(Ordering::SeqCst), 0);
    guard
}

struct Peer {
    child: Child,
    connection: UnixStream,
    folder: PathBuf,
}

struct PendingChild(Option<Child>);
impl Drop for PendingChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}
impl Peer {
    fn new(name: &str) -> Self {
        let root = PathBuf::from(std::env::var_os("ZERUS_MAC_PEER_FIXTURES").unwrap());
        let folder = root.join(format!(
            "case-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        DirBuilder::new().mode(0o700).create(&folder).unwrap();
        let path = folder.join("s");
        assert!(path.as_os_str().as_encoded_bytes().len() < 104);
        let log_path = folder.join("peer.log");
        let log = File::create(&log_path).unwrap();
        let child = Command::new(root.join(name))
            .arg(&path)
            .env("ZERUS_MAC_PEER_EXECUTABLE", root.join("peer-b"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let mut pending = PendingChild(Some(child));
        let deadline = Instant::now() + Duration::from_secs(3);
        let connection = loop {
            if let Ok(connection) = UnixStream::connect(&path) {
                break connection;
            }
            if let Some(status) = pending.0.as_mut().unwrap().try_wait().unwrap() {
                let mut message = String::new();
                let _ = File::open(&log_path)
                    .unwrap()
                    .take(16384)
                    .read_to_string(&mut message);
                panic!("synthetic peer exited before connect: {status}; {message}");
            }
            assert!(Instant::now() < deadline, "synthetic peer did not listen");
            std::thread::sleep(Duration::from_millis(10));
        };
        connection
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        connection
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        Self {
            child: pending.0.take().unwrap(),
            connection,
            folder,
        }
    }
    fn ping(&mut self) {
        self.connection.write_all(b"p").unwrap();
        let mut reply = [0];
        self.connection.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"p");
    }
    fn exit(&mut self) {
        self.connection.write_all(b"q").unwrap();
        assert!(self.child.wait().unwrap().success());
    }
    fn exec(&mut self) {
        self.connection.write_all(b"e").unwrap();
        let mut reply = [0];
        self.connection.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"E");
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        let _ = fs::remove_file(self.folder.join("s"));
        let _ = fs::remove_file(self.folder.join("peer.log"));
        let _ = fs::remove_dir(&self.folder);
    }
}

const IDENTIFIER: &str = "identifier \"com.example.zerus-peer-a\"";
const IDENTIFIER_B: &str = "identifier \"com.example.zerus-peer-b\"";

fn team_requirement() -> String {
    std::env::var("ZERUS_MAC_PEER_TEAM_REQUIREMENT").expect("pinned fixture team requirement")
}

#[test]
#[ignore = "private signed peers and headless Mac heavy lock required"]
fn signed_connection_is_observed_without_consuming_payload_or_ownership() {
    let _serial = serial();
    let mut peer = Peer::new("peer-a");
    {
        let result = observe(&peer.connection, IDENTIFIER).unwrap();
        assert_eq!(
            result.requirement_sha256(),
            <[u8; 32]>::from(Sha256::digest(IDENTIFIER.as_bytes()))
        );
        assert_eq!(format!("{result:?}"), "Observation { .. }");
    }
    assert_eq!(RESOURCES.load(Ordering::SeqCst), 0);
    peer.ping();
}

#[test]
#[ignore = "private signed peers and headless Mac heavy lock required"]
fn team_requirement_rejects_another_team_and_adhoc_peer() {
    let _serial = serial();
    let requirement = team_requirement();
    let accepted = Peer::new("peer-node");
    assert!(observe(&accepted.connection, &requirement).is_ok());
    for name in ["peer-deno", "peer-adhoc"] {
        let mut peer = Peer::new(name);
        assert_eq!(
            observe(&peer.connection, &requirement).unwrap_err(),
            Refusal::LiveCodeRejected
        );
        assert_eq!(RESOURCES.load(Ordering::SeqCst), 0);
        peer.ping();
    }
}

#[test]
#[ignore = "private signed peers and headless Mac heavy lock required"]
fn arbitrary_caller_requirement_is_not_authenticated_installation_trust() {
    let _serial = serial();
    let peer = Peer::new("peer-b");
    assert_eq!(
        observe(&peer.connection, &team_requirement()).unwrap_err(),
        Refusal::LiveCodeRejected
    );
    let supplied = "always";
    let value = observe(&peer.connection, supplied).unwrap();
    assert_eq!(
        value.requirement_sha256(),
        <[u8; 32]>::from(Sha256::digest(supplied.as_bytes()))
    );
    // G1 has no caller here. Its future admission must select the pinned installed
    // requirement itself, rather than trusting arbitrary peer-supplied text.
    assert_eq!(RESOURCES.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "private signed peers and headless Mac heavy lock required"]
fn invalid_requirements_and_failed_checks_release_owned_native_resources() {
    let _serial = serial();
    let mut peer = Peer::new("peer-a");
    for (text, refusal) in [
        ("".to_string(), Refusal::RequirementSize),
        ("x".repeat(REQUIREMENT_BYTES + 1), Refusal::RequirementSize),
        ("always\0never".to_string(), Refusal::RequirementNul),
        ("identifier (".to_string(), Refusal::RequirementInvalid),
        ("never".to_string(), Refusal::LiveCodeRejected),
    ] {
        for _ in 0..8 {
            assert_eq!(observe(&peer.connection, &text).unwrap_err(), refusal);
            assert_eq!(RESOURCES.load(Ordering::SeqCst), 0);
        }
        peer.ping();
    }
    let exact_limit = format!(
        "{IDENTIFIER}{}",
        " ".repeat(REQUIREMENT_BYTES - IDENTIFIER.len())
    );
    let observed = observe(&peer.connection, &exact_limit).unwrap();
    assert_eq!(
        observed.requirement_sha256(),
        <[u8; 32]>::from(Sha256::digest(exact_limit.as_bytes()))
    );
    assert_eq!(RESOURCES.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "private signed peers and headless Mac heavy lock required"]
fn full_token_change_and_kernel_refusal_paths_release_attributes_and_cf_objects() {
    let _serial = serial();
    let peer = Peer::new("peer-a");
    let result = observe_inner(
        &peer.connection,
        IDENTIFIER,
        || {},
        |connection| {
            let mut value = audit(connection)?;
            // Inject an opaque-bit change; do not interpret raw audit fields.
            value.values[0] ^= 1;
            Ok(value)
        },
    );
    assert_eq!(result.unwrap_err(), Refusal::PeerChanged);
    assert_eq!(RESOURCES.load(Ordering::SeqCst), 0);
    for refusal in [Refusal::AuditMalformed, Refusal::AuditUnavailable] {
        assert_eq!(
            observe_inner(&peer.connection, IDENTIFIER, || {}, |_| Err(refusal)).unwrap_err(),
            refusal
        );
        assert_eq!(RESOURCES.load(Ordering::SeqCst), 0);
    }
}

#[test]
#[ignore = "private signed peers and headless Mac heavy lock required"]
fn malformed_kernel_tokens_and_null_create_results_refuse() {
    let _serial = serial();
    let peer = Peer::new("peer-a");
    let token = audit(&peer.connection).unwrap();
    assert!(matches!(
        check_audit(token, 31),
        Err(Refusal::AuditMalformed)
    ));
    assert!(matches!(
        check_audit(AuditToken { values: [0; 8] }, 32),
        Err(Refusal::AuditMalformed)
    ));
    // Public BSM accessors interpret the token; controlled kernel-result test
    // doubles cover each individual zero field without exposing raw identities.
    assert!(unsafe { audit_token_to_pid(token) } > 0);
    assert_ne!(unsafe { audit_token_to_pidversion(token) }, 0);
    assert_eq!(check_lifetime(1, 0), Err(Refusal::AuditMalformed));
    assert_eq!(check_lifetime(0, 1), Err(Refusal::AuditMalformed));
    assert!(matches!(
        unsafe {
            OwnedCf::checked(
                0,
                std::ptr::null_mut(),
                SecCodeGetTypeID(),
                Refusal::LiveCodeUnavailable,
            )
        },
        Err(Refusal::LiveCodeUnavailable)
    ));
    // A valid owned CF string with the wrong type still releases on refusal.
    let wrong = Retained::into_raw(NSString::from_str("synthetic CF result"));
    assert!(matches!(
        unsafe {
            OwnedCf::checked(
                0,
                wrong.cast(),
                SecCodeGetTypeID(),
                Refusal::LiveCodeUnavailable,
            )
        },
        Err(Refusal::LiveCodeUnavailable)
    ));
    let returned_with_error = Retained::into_raw(NSString::from_str("synthetic error result"));
    assert!(matches!(
        unsafe {
            OwnedCf::checked(
                -1,
                returned_with_error.cast(),
                SecCodeGetTypeID(),
                Refusal::LiveCodeUnavailable,
            )
        },
        Err(Refusal::LiveCodeUnavailable)
    ));
    assert_eq!(RESOURCES.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "private signed peers and headless Mac heavy lock required"]
fn exited_peer_and_exit_between_token_reads_never_use_pid_fallback() {
    let _serial = serial();
    let mut peer = Peer::new("peer-a");
    peer.exit();
    let after_exit = observe(&peer.connection, IDENTIFIER);
    eprintln!(
        "exited synthetic peer observation: {:?}",
        after_exit.as_ref().err()
    );
    assert!(matches!(
        after_exit,
        Err(Refusal::LiveCodeUnavailable | Refusal::LiveCodeRejected | Refusal::AuditUnavailable)
    ));
    assert_eq!(RESOURCES.load(Ordering::SeqCst), 0);
    let mut peer = Peer::new("peer-a");
    let connection = peer.connection.try_clone().unwrap();
    let result = observe_inner(&connection, IDENTIFIER, || peer.exit(), audit);
    eprintln!(
        "peer exited between token reads: {:?}",
        result.as_ref().err()
    );
    assert!(matches!(
        result,
        Err(Refusal::LiveCodeUnavailable | Refusal::LiveCodeRejected | Refusal::AuditUnavailable)
    ));
    assert_eq!(RESOURCES.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "private signed peers and headless Mac heavy lock required"]
fn exec_and_reconnect_require_fresh_observation_not_a_cached_permit() {
    let _serial = serial();
    let mut peer = Peer::new("peer-a");
    assert!(observe(&peer.connection, IDENTIFIER).is_ok());
    peer.exec();
    let after_exec = observe(&peer.connection, IDENTIFIER);
    eprintln!(
        "exec on retained connection observation: {:?}",
        after_exec.as_ref().err()
    );
    assert!(after_exec.is_err());
    assert_eq!(RESOURCES.load(Ordering::SeqCst), 0);
    let peer = Peer::new("peer-b");
    assert_eq!(
        observe(&peer.connection, IDENTIFIER).unwrap_err(),
        Refusal::LiveCodeRejected
    );
    assert!(observe(&peer.connection, IDENTIFIER_B).is_ok());
    assert_eq!(RESOURCES.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "private signed peers and headless Mac heavy lock required"]
fn not_connected_socket_refuses_without_a_pid_or_path_lookup() {
    let _serial = serial();
    // SAFETY: create and then transfer ownership of this fresh AF_UNIX socket.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    assert!(fd >= 0);
    let connection = unsafe { UnixStream::from_raw_fd(fd) };
    assert!(observe(&connection, IDENTIFIER).is_err());
    assert_eq!(RESOURCES.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "private signed peers and headless Mac heavy lock required"]
fn dropped_outer_receiver_does_not_release_the_running_check_or_connection() {
    let _serial = serial();
    let mut peer = Peer::new("peer-a");
    let connection = peer.connection.try_clone().unwrap();
    let (entered, waiting) = std::sync::mpsc::sync_channel(1);
    let (release, resume) = std::sync::mpsc::sync_channel(1);
    let (done, result) = std::sync::mpsc::sync_channel(1);
    std::thread::scope(|scope| {
        let connection = &connection;
        let worker = scope.spawn(move || {
            let value = observe_inner(
                connection,
                IDENTIFIER,
                || {
                    entered.send(()).unwrap();
                    resume.recv().unwrap();
                },
                audit,
            )
            .map(|observed| observed.requirement_sha256());
            let _ = done.send(value);
        });
        waiting.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(matches!(
            result.recv_timeout(Duration::from_millis(20)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        drop(result);
        assert!(RESOURCES.load(Ordering::SeqCst) > 0);
        peer.ping();
        release.send(()).unwrap();
        worker.join().unwrap();
    });
    assert_eq!(RESOURCES.load(Ordering::SeqCst), 0);
    peer.ping();
    // This qualifies local helper lifetimes, not G1's absent worker/admission.
}
