//! The final operation alone is synthetic. Every host ownership/protocol method
//! above, including the fixed entry, is the same dormant production method.
use super::super::tests as fixture;
use super::*;
use std::{io::Seek, os::unix::net::UnixListener};
use zerus_client_core::run::GuardedChild;

const KEEPER: &str = "state::effort::picker_scope::host::tests::keeper";
const FRONTEND: &str = "state::effort::picker_scope::host::tests::frontend";
const FIXTURE: &str = "state::effort::picker_scope::host::tests::private_fixture";

#[test]
fn pending_send_has_25_seconds_while_standalone_pickers_keep_15() {
    for (operation, seconds) in [
        (Operation::Send, 25),
        (Operation::Settings, 15),
        (Operation::Effort, 15),
    ] {
        assert_eq!(
            super::super::operation_budget(operation),
            Duration::from_secs(seconds)
        );
    }
}

#[test]
fn send_binary_segment_handles_backpressure_eof_and_its_original_deadline() {
    let (writer, reader) = UnixStream::pair().unwrap();
    writer.set_nonblocking(true).unwrap();
    reader.set_nonblocking(true).unwrap();
    let small = 4096 as libc::c_int;
    assert_eq!(unsafe {
        libc::setsockopt(writer.as_raw_fd(), libc::SOL_SOCKET, libc::SO_SNDBUF,
            (&small as *const libc::c_int).cast(), std::mem::size_of_val(&small) as libc::socklen_t)
    }, 0);
    let bytes = vec![0xa7; 2 << 20];
    let expected = bytes.clone();
    let deadline = Deadline::after(Duration::from_secs(3)).unwrap();
    let sender = std::thread::spawn(move || write_blob(&writer, &bytes, deadline, 2 << 20));
    std::thread::sleep(Duration::from_millis(25));
    assert_eq!(read_blob(&reader, deadline, 2 << 20).unwrap(), expected);
    sender.join().unwrap().unwrap();

    let (peer, reader) = UnixStream::pair().unwrap();
    reader.set_nonblocking(true).unwrap();
    let deadline = Deadline::after(Duration::from_millis(30)).unwrap();
    assert!(read_blob(&reader, deadline, 1024).unwrap_err().contains("deadline expired"));
    drop(peer);
    assert!(read_blob(&reader, Deadline::after(Duration::from_secs(1)).unwrap(), 1024)
        .unwrap_err().contains("endpoint closed"));

    let (mut writer, reader) = UnixStream::pair().unwrap();
    reader.set_nonblocking(true).unwrap();
    writer.write_all(&1025u32.to_be_bytes()).unwrap();
    assert!(read_blob(&reader, Deadline::after(Duration::from_secs(1)).unwrap(), 1024)
        .unwrap_err().contains("exceeds its bound"));
}

pub(super) fn entry_arguments(command: &mut Command) {
    command.args(["--exact", KEEPER, "--ignored", "--test-threads=1"]);
}

fn until() -> Instant {
    Instant::now() + Duration::from_secs(8)
}

fn contended(file: &File) -> bool {
    match file.try_lock_exclusive() {
        Ok(()) => {
            fs2::FileExt::unlock(file).unwrap();
            false
        }
        Err(error) => {
            assert!(crate::platform::lock_contended(&error), "{error}");
            true
        }
    }
}

fn wait(mut condition: impl FnMut() -> bool) {
    let end = until();
    while !condition() {
        assert!(
            Instant::now() < end,
            "private host fixture failed to settle"
        );
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn run(case: &str) {
    for kind in ["fresh", "moved"] {
        let private = tempfile::tempdir().unwrap();
        let mut output = tempfile::tempfile().unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", private.path())
            .env("TMPDIR", private.path())
            .env("HGS_CONFIG_DIR", private.path().join("config"))
            .env("HGS_STATE_DIR", private.path().join("state"))
            .env("ZERUS_PICKER_HOST_TEST", case)
            .env("ZERUS_PICKER_HOST_KIND", kind)
            .args([
                "--exact",
                FIXTURE,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .stdin(Stdio::null())
            .stdout(output.try_clone().unwrap())
            .stderr(output.try_clone().unwrap());
        let mut child = command.spawn().unwrap();
        let end = Instant::now() + Duration::from_secs(25);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= end {
                let _ = child.kill();
                let _ = child.wait();
                panic!("private fixture {kind}/{case} timed out");
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        output.rewind().unwrap();
        let mut text = String::new();
        output.read_to_string(&mut text).unwrap();
        assert!(status.success(), "{kind}/{case}: {text}");
    }
}

#[test]
fn actual_host_retains_same_root_and_sid_after_ordinary_frontend_exit() {
    run("exit");
}
#[test]
fn host_unwind_and_deadline_wait_for_actual_original_drop_completion() {
    run("panic");
    run("expiry");
}
#[test]
fn original_frontend_eof_or_extra_bytes_veto_every_later_effect_and_commit() {
    run("eof");
    run("extra");
}
#[test]
fn one_original_capacity_object_refuses_another_keeper_before_root() {
    run("capacity");
}
#[test]
fn root_acquisition_keeps_the_original_deadline_and_is_cancel_aware() {
    run("root-expiry");
    run("root-eof");
}
#[test]
fn closed_frame_preserves_raw_digest_and_rejects_changed_identity() {
    run("frame");
}

#[test]
fn cold_spawn_turn_and_synchronous_native_callback_cannot_hold_frontend_open() {
    run("spawn-turn");
    run("blocked-native");
}

#[test]
#[ignore = "fixed same-image keeper entry, used only by bounded private fixtures"]
fn keeper() {
    assert!(std::env::var_os("ZERUS_PICKER_HOST_TEST").is_some());
    // A result error must not bypass entry's own physical-completion wait.
    if let Err(error) = crate::cli::dispatch(vec!["__state".into(), ENTRY.into()]) {
        let _ = fs::write(root().join("keeper-error"), error.message);
    }
}

fn raw(record: &Value) -> Vec<u8> {
    format!("{{\n \"request_id\":\"{}\",\"model\":\"example-model\",\"expected_run_id\":\"{}\",\"expected_conversation_id\":\"{}\" }}\n",
        uuid::Uuid::new_v4(), string(record,"run_id"), string(record,"conversation_id")).into_bytes()
}

fn open_writer() -> File {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root().join(".lock"))
        .unwrap()
}

#[test]
#[ignore = "one ordinary frontend main; returns normally while original host drains"]
fn frontend() {
    let path = std::env::var_os("ZERUS_PICKER_HOST_FRAME").unwrap();
    let frame: Frame = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let mut front = Frontend::launch(frame).unwrap_or_else(|error| {
        let _ = fs::write(root().join("frontend-error"), &error);
        panic!("{error}");
    });
    fs::write(root().join("frontend-ready"), front.child.id().to_string()).unwrap();
    let mut command = [0];
    std::io::stdin().read_exact(&mut command).unwrap();
    match command[0] {
        b'x' => {
            front.stream.write_all(b"unexpected").unwrap();
            // Keep the frontend alive: extra bytes alone must revoke barriers.
            let mut exit = [0];
            std::io::stdin().read_exact(&mut exit).unwrap();
            assert_eq!(exit[0], b'e');
        }
        b'e' => {}
        _ => panic!("unknown private frontend instruction"),
    }
    assert!(front.offered);
    // Normal Rust/libtest main exit, no process signal. Drop closes this ONE
    // original endpoint; it never kills the host or its native helper.
    drop(front);
}

pub(super) fn operation(
    _frame: &Frame,
    scope: Scope<'static>,
    _control: Arc<Control>,
) -> Result<Vec<u8>> {
    let case = std::env::var("ZERUS_PICKER_HOST_TEST").unwrap();
    if case == "blocked-native" {
        let mut scope = scope;
        let mut delivered = false;
        assert!(scope
            .key(
                Key::Escape,
                |_| {
                    fs::write(
                        root().join("native-entered"),
                        b"before native observation returns",
                    )
                    .unwrap();
                    wait(|| root().join("native-release").exists());
                    Ok(())
                },
                |_, _| {
                    delivered = true;
                    Ok(())
                }
            )
            .is_err());
        assert!(
            !delivered,
            "EOF after a synchronous native observation vetoes the later effect"
        );
        return Err("private blocked native observation settled after EOF".into());
    }
    let prepared = super::super::helper::Prepared::from_scope(scope)?;
    let socket = PathBuf::from(std::env::var_os("ZERUS_PICKER_HELPER_SOCKET").unwrap());
    let effect = PathBuf::from(std::env::var_os("ZERUS_PICKER_HELPER_EFFECT").unwrap());
    let mut helper = prepared.spawn(
        |_| Ok(()),
        |_, _| {
            Ok(GuardedChild::Ready(fixture::helper::child(
                "hold", &socket, &effect,
            )))
        },
    )?;
    if !matches!(case.as_str(), "eof" | "extra") {
        wait(|| root().join("retire-helper").exists());
        fs::write(root().join("retired-helper"), b"same attached original").unwrap();
    }
    if case == "panic" {
        panic!("private final operation panic after exact helper attachment");
    }
    if matches!(case.as_str(), "eof" | "extra") {
        // Wait for the driver to close the original descendant endpoint. By
        // then the frontend has either exited0 or sent the unexpected byte.
        let end = until();
        let mut drained = loop {
            if let Some(drained) = helper.poll() {
                break drained;
            }
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(2));
        };
        let mut calls = 0;
        assert!(drained
            .scope
            .key(
                Key::Escape,
                |_| {
                    calls += 1;
                    Ok(())
                },
                |_, _| Ok(())
            )
            .is_err());
        assert!(drained
            .scope
            .record_commit(
                |_| {
                    calls += 1;
                    Ok(())
                },
                |_, _, _| Ok(())
            )
            .is_err());
        assert!(drained
            .scope
            .receipt_commit(
                |_| {
                    calls += 1;
                    Ok(())
                },
                |_, _| Ok(())
            )
            .is_err());
        assert_eq!(calls, 0);
        fs::write(
            root().join("all-barriers-refused"),
            b"original EOF/extra byte",
        )
        .unwrap();
        drop(drained);
    }
    drop(helper);
    Err("private synthetic operation offers no native completion".into())
}

#[test]
#[ignore = "private isolated host fixture, invoked only by public controls"]
fn private_fixture() {
    let case = std::env::var("ZERUS_PICKER_HOST_TEST").unwrap();
    let kind = std::env::var("ZERUS_PICKER_HOST_KIND").unwrap();
    let (writer, record) = fixture::setup(&kind);
    let original_root = open_writer();
    let original_sid = fixture::input(&record);
    let root_id = crate::platform::FileIdentity::of(&original_root).unwrap();
    let sid_id = crate::platform::FileIdentity::of(&original_sid).unwrap();
    let duration = if matches!(case.as_str(), "expiry" | "root-expiry" | "spawn-turn") {
        Duration::from_millis(700)
    } else {
        Duration::from_secs(12)
    };
    let frame = Frame::new(
        Operation::Settings,
        raw(&record),
        &record,
        Deadline::after(duration).unwrap(),
    )
    .unwrap();
    if case == "frame" {
        frame_controls(frame, writer);
        return;
    }
    if case == "spawn-turn" {
        let turn = crate::platform::spawn_turn();
        let start = Instant::now();
        assert!(Frontend::launch(frame).is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(
            Slot::reserve().is_err(),
            "blocked cold worker keeps its original capacity charge"
        );
        drop(turn);
        wait(|| Slot::reserve().is_ok());
        assert!(!root().join("argv-effect").exists());
        drop(writer);
        return;
    }
    let frame_file = root().join("original-frame");
    fs::write(&frame_file, serde_json::to_vec(&frame).unwrap()).unwrap();
    let socket = root().parent().unwrap().join("host-descendant.sock");
    let effect = root().join("argv-effect");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .env("ZERUS_PICKER_HOST_FRAME", &frame_file)
        .env("ZERUS_PICKER_HELPER_SOCKET", &socket)
        .env("ZERUS_PICKER_HELPER_EFFECT", &effect)
        .args(["--exact", FRONTEND, "--ignored", "--test-threads=1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut frontend = command.spawn().unwrap();
    if matches!(case.as_str(), "root-expiry" | "root-eof") {
        wait(|| root().join("frontend-ready").exists());
        if case == "root-expiry" {
            std::thread::sleep(Duration::from_millis(850));
        }
        frontend.stdin.as_mut().unwrap().write_all(b"e").unwrap();
        wait(|| frontend.try_wait().unwrap().is_some());
        assert!(frontend.wait().unwrap().success());
        assert!(!effect.exists(), "busy Root must not admit a helper");
        drop(writer);
        wait(|| Slot::reserve().is_ok());
        assert!(
            !effect.exists(),
            "EOF/deadline cannot renew after Root becomes free"
        );
        return;
    }
    drop(writer);
    if case == "blocked-native" {
        wait(|| root().join("native-entered").exists());
        wait(|| root().join("frontend-ready").exists());
        assert!(contended(&original_root) && contended(&original_sid));
        frontend.stdin.as_mut().unwrap().write_all(b"e").unwrap();
        wait(|| frontend.try_wait().unwrap().is_some());
        assert!(frontend.wait().unwrap().success());
        assert!(contended(&original_root) && contended(&original_sid));
        fs::write(root().join("native-release"), b"exact private release").unwrap();
        wait(|| !contended(&original_root) && !contended(&original_sid));
        wait(|| Slot::reserve().is_ok());
        assert!(!effect.exists());
        return;
    }
    let peer = {
        let end = until();
        loop {
            match listener.accept() {
                Ok((mut peer, _)) => {
                    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    let mut ready = [0; 5];
                    peer.read_exact(&mut ready).unwrap();
                    assert_eq!(&ready, b"ready");
                    break peer;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("private descendant: {error}"),
            }
            assert!(
                Instant::now() < end,
                "private host never offered original helper; keeper={:?}; frontend={:?}",
                fs::read_to_string(root().join("keeper-error")),
                fs::read_to_string(root().join("frontend-error"))
            );
            std::thread::sleep(Duration::from_millis(3));
        }
    };
    wait(|| root().join("frontend-ready").exists());
    if !matches!(case.as_str(), "eof" | "extra") {
        fs::write(
            root().join("retire-helper"),
            b"driver owns descendant endpoint",
        )
        .unwrap();
        wait(|| root().join("retired-helper").exists());
    }
    assert!(contended(&original_root) && contended(&original_sid));
    if case == "capacity" {
        assert!(
            Frontend::launch(frame.clone()).is_err(),
            "one global original slot refuses another keeper"
        );
    }
    frontend
        .stdin
        .as_mut()
        .unwrap()
        .write_all(if case == "extra" { b"x" } else { b"e" })
        .unwrap();
    if case != "extra" {
        wait(|| frontend.try_wait().unwrap().is_some());
        assert!(
            frontend.wait().unwrap().success(),
            "ordinary frontend must exit0"
        );
    }
    if case == "expiry" {
        std::thread::sleep(Duration::from_millis(850));
    }
    // Parent's command main is already gone (or the extra-byte frontend is
    // still alive). Neither condition releases this actual Root/SID source.
    assert!(contended(&original_root) && contended(&original_sid));
    assert_eq!(
        root_id,
        crate::platform::FileIdentity::of(&original_root).unwrap()
    );
    assert_eq!(
        sid_id,
        crate::platform::FileIdentity::of(&original_sid).unwrap()
    );
    assert!(Slot::reserve().is_err());
    let mut byte = 0u8;
    assert_eq!(
        unsafe {
            libc::recv(
                peer.as_raw_fd(),
                (&mut byte as *mut u8).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().kind(),
        std::io::ErrorKind::WouldBlock
    );
    drop(peer); // exact original descendant endpoint, never PID/group kill
    if matches!(case.as_str(), "eof" | "extra") {
        wait(|| root().join("all-barriers-refused").exists());
    }
    wait(|| !contended(&original_root) && !contended(&original_sid));
    wait(|| Slot::reserve().is_ok());
    if case == "extra" {
        frontend.stdin.as_mut().unwrap().write_all(b"e").unwrap();
        wait(|| frontend.try_wait().unwrap().is_some());
    }
    // Every branch has observed actual exit above; this is the same Child's
    // cached/reaped status, and keeps sole-waiter ownership explicit.
    assert!(frontend.wait().unwrap().success());
}

fn frame_controls(frame: Frame, writer: Guard) {
    assert!(crate::state::dispatch(&[ENTRY.into(), "extra".into()]).is_err());
    assert!(crate::cli::dispatch(vec!["__state".into(), ENTRY.into(), "extra".into()]).is_err());
    assert!(frame.raw.ends_with(b" }\n"));
    assert_eq!(
        Frame::new(
            frame.operation,
            frame.raw.clone(),
            &frame.binding,
            frame.deadline
        )
        .unwrap()
        .raw,
        frame.raw
    );
    let mut wrong = frame.clone();
    wrong.binding["run_id"] = json!(uuid::Uuid::new_v4().to_string());
    assert!(wrong.validate().is_err());
    let mut wrong = frame.clone();
    wrong.image.file.1 ^= 1;
    assert!(wrong.check_context().is_err());
    let mut wrong = frame.clone();
    wrong.raw.resize(4097, b' ');
    assert!(wrong.validate().is_err());
    let mut wrong = frame.clone();
    wrong.configuration = "0".repeat(64);
    assert!(wrong.check_context().is_err());
    let mut encoded = serde_json::to_value(&frame).unwrap();
    encoded["argv"] = json!(["unexpected"]);
    assert!(serde_json::from_value::<Frame>(encoded).is_err());
    let receipts = Directory::open(&crate::platform::canonical(root()).unwrap())
        .unwrap()
        .child("settings_receipts")
        .unwrap();
    let digest = format!("{:x}", Sha256::digest(&frame.raw));
    receipts
        .write_new(
            &format!("{}.json", frame.request_id),
            serde_json::to_vec(
                &json!({"name":frame.binding["name"],"digest":digest,"status":"unknown"}),
            )
            .unwrap()
            .as_slice(),
        )
        .unwrap();
    assert!(
        original_receipt(&frame).is_err(),
        "unknown original receipt never authorizes replay"
    );
    let mut changed = frame.clone();
    changed.raw.push(b' ');
    assert!(
        original_receipt(&changed).is_err(),
        "exact raw content participates in original receipt digest"
    );
    drop(writer);
}
