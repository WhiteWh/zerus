use super::*;
use std::fs::OpenOptions;

#[test]
fn a_timed_out_setup_keeps_the_actual_owner_until_the_original_worker_joins() {
    let root = tempfile::tempdir().unwrap();
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(root.path().join("owner"))
        .unwrap();
    let owner = Arc::new(crate::platform::FileLock::exclusive(file).unwrap());
    let weak = Arc::downgrade(&owner);
    let (entered, ready) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    let reference = Reference::capture_with(&owner, Duration::from_millis(30), move || {
        entered.send(()).unwrap();
        wait.recv().unwrap();
        Err(std::io::ErrorKind::PermissionDenied.into())
    });
    ready.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(reference.is_none());
    drop(owner);
    assert!(
        weak.upgrade().is_some(),
        "caller timeout cannot release the copy worker's owner"
    );
    release.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while weak.upgrade().is_some() && Instant::now() < until {
        thread::yield_now();
    }
    assert!(
        weak.upgrade().is_none(),
        "original join must release the owner"
    );
}

#[test]
fn failed_or_panicked_setup_never_publishes_a_reference_or_keeps_an_owner() {
    for panic in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(root.path().join("owner"))
            .unwrap();
        let owner = Arc::new(crate::platform::FileLock::exclusive(file).unwrap());
        assert!(
            Reference::capture_with(&owner, Duration::from_secs(1), move || {
                assert!(!panic, "private copy-worker failure");
                Err(std::io::ErrorKind::PermissionDenied.into())
            })
            .is_none()
        );
        let until = Instant::now() + Duration::from_secs(1);
        while Arc::strong_count(&owner) > 1 && Instant::now() < until {
            thread::yield_now();
        }
        assert_eq!(Arc::strong_count(&owner), 1);
    }
}

const CHILD: &str = "desktop::ui_authority::linux_image::tests::native_child";
const MODE: &str = "ZERUS_SEALED_RELAY_CONTROL";
struct Owned(std::process::Child);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn connection(listener: &std::os::unix::net::UnixListener) -> std::os::unix::net::UnixStream {
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                return stream;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until => {
                thread::yield_now()
            }
            other => panic!("original child did not reach phase: {other:?}"),
        }
    }
}

#[test]
fn native_child() {
    use super::super::{Freshness, Metadata, Validated};
    use std::io::{Read, Write};
    use std::os::{
        fd::AsRawFd,
        unix::{
            net::{UnixListener, UnixStream},
            process::CommandExt,
        },
    };
    use std::process::{Command, Stdio};
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    if mode == "first" || mode == "second" {
        let mut wire =
            UnixStream::connect(std::env::var_os("ZERUS_SEALED_CONTROL_SOCKET").unwrap()).unwrap();
        wire.write_all(if mode == "first" { b"1" } else { b"2" })
            .unwrap();
        if mode == "first" {
            let mut next = [0];
            wire.read_exact(&mut next).unwrap();
            assert_eq!(next, b"E"[..]);
            let fd = std::env::var("ZERUS_SEALED_CONTROL_FD").unwrap();
            let error = Command::new(format!("/proc/self/fd/{fd}"))
                .args(["--exact", CHILD, "--nocapture"])
                .env(MODE, "second")
                .exec();
            panic!("second exec refused: {error}");
        }
        let mut eof = [0];
        assert_eq!(std::io::stdin().read(&mut eof).unwrap(), 0);
        return;
    }
    assert_eq!(mode, "exercise");
    let root = tempfile::tempdir().unwrap();
    let owner_file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(root.path().join("owner"))
        .unwrap();
    let owner = Arc::new(crate::platform::FileLock::exclusive(owner_file).unwrap());
    let reference = Reference::capture(&owner)
        .expect("private stripped native reference must fit the original cap");
    let (release, wait) = mpsc::sync_channel(1);
    let late = Reference::capture_with(&owner, Duration::from_millis(20), move || {
        wait.recv().unwrap();
        ServiceImage::capture()
    });
    assert!(late.is_none());
    assert!(Arc::strong_count(&owner) > 1);
    release.send(()).unwrap();
    let settled = Instant::now() + Duration::from_secs(2);
    while Arc::strong_count(&owner) > 1 && Instant::now() < settled {
        thread::yield_now();
    }
    assert_eq!(
        Arc::strong_count(&owner),
        1,
        "late successful setup must join and drop its unpublished snapshot"
    );
    let path = std::env::current_exe().unwrap();
    let first = zerus_client_core::image::linux::Pin::open(&path).unwrap();
    let second = zerus_client_core::image::linux::Pin::open(&path).unwrap();
    let next = File::open(second.executable()).unwrap();
    let fd = next.as_raw_fd();
    let socket = root.path().join("control");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut command = Command::new(first.executable());
    command
        .args(["--exact", CHILD, "--nocapture"])
        .env(MODE, "first")
        .env("ZERUS_SEALED_CONTROL_SOCKET", &socket)
        .env("ZERUS_SEALED_CONTROL_FD", fd.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    first.inherit(&mut command).unwrap();
    // SAFETY: only fcntl on one retained fixture-owned descriptor after fork.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = Owned(command.spawn().unwrap());
    let mut original_wire = connection(&listener);
    let mut phase = [0];
    original_wire.read_exact(&mut phase).unwrap();
    assert_eq!(phase, b"1"[..]);
    let mut caller = Process::read(child.0.id()).unwrap();
    assert!(reference.observe(&caller, Instant::now()).is_err());
    assert_eq!(
        Arc::strong_count(&owner),
        1,
        "expired auth must release the original owner loan"
    );
    caller.linux_freshness = Some(Arc::new(super::super::LinuxFreshness::Service(
        reference
            .observe(&caller, Instant::now() + Duration::from_secs(2))
            .unwrap(),
    )));
    let original = caller.clone();
    use zerus_client_core::{
        Target,
        desktop::termination_outcome::{Choice, Intent, Scope},
        termination_cleanup::Capture,
    };
    let capture = Capture {
        target: Target {
            computer: String::new(),
            name: "codex/example/source".into(),
            run_id: "run".into(),
            conversation_id: "conversation".into(),
        },
        machine: uuid::Uuid::new_v4().to_string(),
        root: "/srv/example/work".into(),
        common_dir: "/srv/example/main/.git".into(),
        owner_id: "original-owner".into(),
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let original_input =
        String::from_utf8(capture.invocation(&request_id).unwrap().stdin.unwrap()).unwrap();
    let request = zerus_client_core::desktop::ui_requests::Request::TerminationOutcome {
        intent: Box::new(Intent {
            scope: Scope {
                configuration: "a".repeat(64),
                service_epoch: uuid::Uuid::new_v4().to_string(),
            },
            confirmation_id: uuid::Uuid::new_v4().to_string(),
            choice: Choice::RemoveAfterTerminate,
            request_id,
            capture,
            original: original_input,
        }),
    };
    // Synthetic native permit isolates SAME-child kernel freshness. Genuine
    // canonical/frontend/config/ancestry admission is in the CLI integration.
    let permit = Validated(
        request,
        Freshness::Native {
            caller,
            frontend: Process::read(std::process::id()).unwrap(),
            until: Instant::now() + Duration::from_secs(2),
        },
        Metadata::default(),
    );
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    permit.termination_outcome_parts().unwrap();
    original_wire.write_all(b"E").unwrap();
    let mut replaced_wire = connection(&listener);
    replaced_wire.read_exact(&mut phase).unwrap();
    assert_eq!(phase, b"2"[..]);
    let replaced = Process::read(child.0.id()).unwrap();
    assert_eq!(
        original.identity, replaced.identity,
        "exec must preserve original PID/birth/parent"
    );
    assert_eq!(
        original.executable, replaced.executable,
        "SAME memfd display name control must really hold"
    );
    assert!(
        !original.current(),
        "retaining the old fd cannot prove current code"
    );
    assert!(
        permit.termination_outcome_parts().is_err(),
        "the AFTER-BEGIN check must reobserve the original kernel exe"
    );
    db.execute_batch("ROLLBACK").unwrap();
    drop(original);
    drop(permit);
    assert_eq!(
        Arc::strong_count(&owner),
        1,
        "refusal must release the original peer's owner loan"
    );
    drop(child.0.stdin.take());
    assert!(child.0.wait().unwrap().success());
}

#[test]
fn exec_to_another_same_name_sealed_image_is_refused_at_the_real_after_begin_check() {
    use std::process::{Command, Stdio};
    let root = tempfile::tempdir().unwrap();
    let helper = root.path().join("native-control");
    std::fs::copy(std::env::current_exe().unwrap(), &helper).unwrap();
    // Hosted debug test binaries exceed256MiB. Strip only this private copy;
    // BOTH the actual service reference and relay use that SAME native image.
    // The symbol tables go too: with debug sections alone removed the test
    // binary passed the bound once the suite had grown (258.5 MiB).
    let stripped = Command::new("strip")
        .arg("--strip-all")
        .arg(&helper)
        .output()
        .unwrap();
    assert!(
        stripped.status.success(),
        "{}",
        String::from_utf8_lossy(&stripped.stderr)
    );
    assert!(std::fs::metadata(&helper).unwrap().len() < 256 << 20);
    let mut child = Owned(
        Command::new(helper)
            .args(["--exact", CHILD, "--nocapture"])
            .env(MODE, "exercise")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            use std::io::Read;
            let mut error = String::new();
            child
                .0
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut error)
                .unwrap();
            assert!(status.success(), "private original-child control: {error}");
            break;
        }
        assert!(
            Instant::now() < until,
            "private native control exceeded its whole bound"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
