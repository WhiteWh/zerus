use super::*;
#[cfg(target_os = "linux")]
static CLIENT_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

thread_local! {
    static RENAME_ERROR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static RENAMED: std::cell::RefCell<Option<std::path::PathBuf>> = const { std::cell::RefCell::new(None) };
}
pub(super) fn claim_move(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)?;
    if RENAME_ERROR.with(|enabled| enabled.replace(false)) {
        RENAMED.with(|owned| *owned.borrow_mut() = Some(to.to_path_buf()));
        return Err(io::Error::other("fixture unknown rename outcome"));
    }
    Ok(())
}

struct Fixture {
    _temp: tempfile::TempDir,
    location: Location,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let location = Location {
            root: zerus_desktop_migration::canonical(&temp.path().join("state/desktop")).unwrap(),
            configuration: "a".repeat(64),
        };
        Self {
            _temp: temp,
            location,
        }
    }
}
fn staged(location: &Location, id: &str) -> Staged {
    let bytes = String::from_utf8(super::super::tests::bundle(""))
        .unwrap()
        .replace("11111111-1111-4111-8111-111111111111", id)
        .replace(&"a".repeat(64), &location.configuration);
    stage(location, &mut bytes.as_bytes()).unwrap()
}
fn transfer(staged: Staged) -> (Reference, std::path::PathBuf) {
    let directory = key(&directory_handle(staged.stage.path()).unwrap()).unwrap();
    let reference = Reference {
        version: 1,
        stage: staged
            .stage
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .into(),
        import_id: staged.start.import_id.clone(),
        record_sha256: sha256(&staged.manifest),
        max_bytes: staged.start.max_bytes,
        directory,
    };
    (reference, staged.stage.keep())
}
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[test]
fn owned_stage_is_revalidated_then_published_once_without_releasing_the_lock() {
    let f = Fixture::new();
    let import = id();
    let input = staged(&f.location, &import);
    let expected = input.receipt();
    let (reference, original) = transfer(input);
    let (repeat, _) = transfer(staged(&f.location, &import));
    let lock = f.location.handoff_lock().unwrap();
    let stopped = AtomicBool::new(false);
    let result = finalize(&f.location, &reference, &lock, &stopped, |owned| {
        assert!(!original.exists());
        assert!(owned.join("record-v1.json").is_file());
        assert!(f.location.handoff_lock().is_err());
        Ok(())
    })
    .unwrap();
    assert_eq!(result, expected);
    assert_eq!(
        status(&f.location, &import).unwrap()["record_present"],
        true
    );
    let record = f
        .location
        .root
        .join("imports")
        .join(&import)
        .join("record-v1.json");
    let bytes = fs::read(&record).unwrap();
    assert!(finalize(&f.location, &repeat, &lock, &stopped, |_| Ok(())).is_err());
    assert_eq!(fs::read(record).unwrap(), bytes);
    assert!(f.location.handoff_lock().is_err());
}

#[test]
fn server_claim_and_commit_work_with_the_original_cli_directory_handle_still_open() {
    let f = Fixture::new();
    let import = id();
    let input = staged(&f.location, &import);
    let expected = input.receipt();
    // This is route()'s actual CLI handle, including FILE_SHARE_DELETE on
    // Windows. finalize() also keeps its own original directory handle open
    // across the production custody rename, before opening Replay's children.
    let cli_pin = directory_handle(input.stage.path()).unwrap();
    let (reference, original) = transfer(input);
    let lock = f.location.handoff_lock().unwrap();
    let result = finalize(
        &f.location,
        &reference,
        &lock,
        &AtomicBool::new(false),
        |owned| {
            assert!(!original.exists());
            assert_eq!(key(&cli_pin).unwrap(), reference.directory);
            assert_eq!(
                key(&directory_handle(owned).unwrap()).unwrap(),
                reference.directory
            );
            assert!(owned.join("record-v1.json").is_file());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(result, expected);
    assert_eq!(key(&cli_pin).unwrap(), reference.directory);
    assert_eq!(
        status(&f.location, &import).unwrap()["record_present"],
        true
    );
    assert!(f.location.handoff_lock().is_err());
}

#[test]
fn malformed_replaced_linked_and_over_limit_stages_never_reserve_a_uuid() {
    for change in [
        "directory",
        "record",
        "extra",
        "page",
        "limit",
        "hardlink",
        "symlink",
    ] {
        let f = Fixture::new();
        let import = id();
        let (mut reference, original) = transfer(staged(&f.location, &import));
        let lock = f.location.handoff_lock().unwrap();
        match change {
            "directory" => reference.directory[1] ^= 1,
            "record" => reference.record_sha256 = "0".repeat(64),
            "limit" => reference.max_bytes = 1,
            "extra" => private_write(&original.join("extra"), b"null").unwrap(),
            "page" => {
                let page = fs::read_dir(original.join("pages"))
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                fs::write(page, b"null").unwrap();
            }
            "hardlink" => {
                fs::hard_link(
                    original.join("record-v1.json"),
                    f._temp.path().join("other-link"),
                )
                .unwrap();
            }
            "symlink" => {
                #[cfg(unix)]
                {
                    let path = original.join("record-v1.json");
                    let other = f._temp.path().join("other-record");
                    fs::rename(&path, &other).unwrap();
                    std::os::unix::fs::symlink(other, path).unwrap();
                }
                #[cfg(not(unix))]
                {
                    continue;
                }
            }
            _ => unreachable!(),
        }
        let failure = finalize(
            &f.location,
            &reference,
            &lock,
            &AtomicBool::new(false),
            |_| Ok(()),
        )
        .unwrap_err();
        assert_eq!(failure.rename_entered, change != "directory", "{change}");
        assert!(
            !f.location.root.join("imports").join(import).exists(),
            "{change}"
        );
    }
}

#[test]
fn transfer_failure_and_panic_preserve_the_admitted_input_without_reserving() {
    for panic in [false, true] {
        let f = Fixture::new();
        let import = id();
        let (reference, original) = transfer(staged(&f.location, &import));
        let lock = f.location.handoff_lock().unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            finalize(
                &f.location,
                &reference,
                &lock,
                &AtomicBool::new(false),
                |owned| {
                    assert!(!original.exists());
                    assert!(owned.join("record-v1.json").exists());
                    if panic {
                        panic!("original worker fixture panic");
                    }
                    anyhow::bail!("original worker fixture failure")
                },
            )
        }));
        if panic {
            assert!(result.is_err());
        } else {
            assert!(
                result.unwrap().unwrap_err().rename_entered,
                "a failure after custody transfer stays unknown"
            );
        }
        assert!(!f.location.root.join("imports").join(import).exists());
        let retained: Vec<_> = fs::read_dir(f.location.root.join("imports"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(".held-import-")
            })
            .collect();
        assert_eq!(retained.len(), 1);
        assert!(retained[0].join("input/record-v1.json").is_file());
    }
}

#[test]
fn replay_refuses_same_byte_file_replacement_directory_replacement_and_expired_work() {
    for change in ["file", "directory", "deadline", "stopped"] {
        let f = Fixture::new();
        let import = id();
        let (reference, original) = transfer(staged(&f.location, &import));
        let stopped = AtomicBool::new(change == "stopped");
        let until = if change == "deadline" {
            Instant::now()
        } else {
            Instant::now() + TOTAL
        };
        let replay = Replay::new(&original, &reference, &f.location, &stopped, until);
        if matches!(change, "deadline" | "stopped") {
            assert!(replay.is_err());
            continue;
        }
        let replay = replay.unwrap();
        if change == "file" {
            let leaf = fs::read_dir(original.join("snapshots"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let bytes = fs::read(&leaf).unwrap();
            // Keep the original inode allocated: replacement identity cannot
            // accidentally be recycled into an apparently unchanged file.
            fs::rename(&leaf, f._temp.path().join("original-body")).unwrap();
            private_write(&leaf, &bytes).unwrap();
        } else {
            let replaced = fs::rename(&original, f._temp.path().join("original-directory"));
            #[cfg(windows)]
            {
                // Replay pins child directories as well as this root. Windows
                // refuses replacing that tree while those handles are open;
                // the OS refusal itself preserves the original input.
                let error = replaced.expect_err("Windows must protect the pinned replay tree");
                assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                assert_eq!(
                    key(&directory_handle(&original).unwrap()).unwrap(),
                    reference.directory
                );
                let unchanged = stage(&f.location, &mut BufReader::new(replay)).unwrap();
                assert_eq!(sha256(&unchanged.manifest), reference.record_sha256);
                assert!(!f.location.root.join("imports").join(import).exists());
                continue;
            }
            #[cfg(not(windows))]
            {
                replaced.unwrap();
                directory(&original).unwrap();
            }
        }
        assert!(
            stage(&f.location, &mut BufReader::new(replay)).is_err(),
            "{change}"
        );
        assert!(!f.location.root.join("imports").join(import).exists());
    }
}

#[test]
fn managed_reference_has_a_closed_small_scope_and_a_finite_budget() {
    let f = Fixture::new();
    let (reference, _) = transfer(staged(&f.location, &id()));
    let value = json!({"version":1,"stage":reference.stage,"import_id":reference.import_id,
        "record_sha256":reference.record_sha256,"max_bytes":reference.max_bytes,"directory":reference.directory});
    assert!(Reference::parse(value.clone()).is_ok());
    for (field, changed) in [
        ("stage", json!("../../outside")),
        ("stage", json!(".stage-/outside")),
        ("max_bytes", json!(MAX_BYTES + 1)),
        ("max_bytes", json!(0)),
        ("record_sha256", json!("bad")),
        ("role", json!("owner")),
        ("path", json!("/private/caller")),
        ("import_id", json!("bad")),
    ] {
        let mut invalid = value.clone();
        invalid[field] = changed;
        assert!(Reference::parse(invalid).is_err(), "{field}");
    }
}

#[test]
#[cfg(target_os = "linux")]
fn cli_route_cleans_only_a_complete_exact_negative_and_retains_unknown_input() {
    for mode in [
        "negative",
        "cleanup_refused",
        "mismatched",
        "partial",
        "disconnect",
    ] {
        let f = Fixture::new();
        f.location.prepare().unwrap();
        let lock = f.location.handoff_lock().unwrap();
        let listener = crate::desktop::transport::Listener::bind(&f.location).unwrap();
        let location = Location {
            root: f.location.root.clone(),
            configuration: f.location.configuration.clone(),
        };
        let server = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(2);
            let mut stream = loop {
                match listener.accept() {
                    Ok(stream) => break stream,
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < until =>
                    {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("private fixture accept: {error}"),
                }
            };
            crate::desktop::transport::authenticate(&stream).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let bytes = protocol::line(
                &mut BufReader::new(stream.try_clone().unwrap()),
                protocol::CONTROL_BYTES,
            )
            .unwrap()
            .unwrap();
            let hello: protocol::Hello = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(hello.op, OP);
            let reference = Reference::parse(hello.request.unwrap()).unwrap();
            let original = location.root.join("imports").join(&reference.stage);
            assert!(
                original.join("record-v1.json").is_file(),
                "custody survives the CLI TempDir relinquishment before IPC"
            );
            if mode == "disconnect" {
                return;
            }
            if mode == "cleanup_refused" {
                private_write(&original.join("unknown.txt"), b"keep for review").unwrap();
            }
            let mut value = reference.not_committed(&location);
            if mode == "mismatched" {
                value["import_record"]["sha256"] = "0".repeat(64).into();
            }
            let mut bytes = protocol::encode(&crate::desktop::frame(
                "original-epoch",
                1,
                "response",
                value,
            ))
            .unwrap();
            if mode == "partial" {
                bytes.truncate(bytes.len() - 2);
            }
            stream.write_all(&bytes).unwrap();
        });
        let result = publish(&f.location, &mut &super::super::tests::bundle("")[..]);
        server.join().unwrap();
        let stages: Vec<_> = fs::read_dir(f.location.root.join("imports"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(".stage-")
            })
            .collect();
        if matches!(mode, "negative" | "cleanup_refused") {
            assert_eq!(result.unwrap()["this_attempt_not_committed"], true);
            if mode == "negative" {
                assert!(stages.is_empty());
            } else {
                assert_eq!(stages.len(), 1);
                assert_eq!(
                    fs::read(stages[0].join("unknown.txt")).unwrap(),
                    b"keep for review"
                );
            }
        } else {
            assert!(result.is_err(), "{mode}");
            assert_eq!(stages.len(), 1, "{mode}");
            assert!(stages[0].join("record-v1.json").is_file());
        }
        assert!(f.location.handoff_lock().is_err());
        drop(lock);
    }
}

#[cfg(target_os = "linux")]
pub(in crate::desktop) struct Pending {
    pub owned: std::path::PathBuf,
    pub import_id: String,
    peer: Option<Stream>,
    release: mpsc::SyncSender<()>,
    worker: std::thread::JoinHandle<Result<()>>,
    _serial: std::sync::MutexGuard<'static, ()>,
}
#[cfg(target_os = "linux")]
impl Pending {
    pub fn disconnect(&mut self) {
        self.peer = None;
    }
    pub fn finish_connected(mut self) -> Value {
        self.release.send(()).unwrap();
        let peer = self.peer.take().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let bytes = protocol::line(&mut BufReader::new(peer), protocol::FRAME_BYTES)
            .unwrap()
            .unwrap();
        self.worker.join().unwrap().unwrap();
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response["t"], "response");
        assert_eq!(response["value"]["published"], true);
        assert_eq!(response["value"]["import_id"], self.import_id);
        assert!(Admission::take().is_ok());
        response["value"].clone()
    }
    pub fn finish(self) {
        self.release.send(()).unwrap();
        assert!(
            self.worker.join().unwrap().is_err(),
            "disconnected caller cannot receive a success"
        );
    }
}
/// The real granted-runtime test supplies its actor-selected original Owner.
/// The existing client thread then holds it across transfer, disconnect and
/// revoke, exercising the same production finalizer rather than a fake owner.
#[cfg(target_os = "linux")]
pub(in crate::desktop) fn pending(location: &Location, owner: Arc<Owner>) -> Pending {
    let serial = CLIENT_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let import_id = id();
    let (reference, original) = transfer(staged(location, &import_id));
    let request = json!({"version":1,"stage":reference.stage,"import_id":reference.import_id,
        "record_sha256":reference.record_sha256,"max_bytes":reference.max_bytes,"directory":reference.directory});
    let hello: protocol::Hello = serde_json::from_value(json!({"abi":protocol::ABI,
        "configuration":location.configuration,"op":OP,"request":request}))
    .unwrap();
    let (peer, stream) = std::os::unix::net::UnixStream::pair().unwrap();
    let (send, receive) = mpsc::sync_channel(1);
    let (started, observed) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    let configuration = location.configuration.clone();
    let location = Location {
        root: location.root.clone(),
        configuration: configuration.clone(),
    };
    let worker = std::thread::spawn(move || {
        client_with(
            stream,
            hello,
            send,
            Arc::new(AtomicBool::new(false)),
            &location,
            |owned| {
                assert!(!original.exists());
                started.send(owned.to_path_buf()).unwrap();
                wait.recv_timeout(Duration::from_secs(10)).unwrap();
                Ok(())
            },
        )
    });
    match receive.recv_timeout(Duration::from_secs(2)).unwrap() {
        super::super::super::Event::ImportOwner(request_configuration, reply) => {
            assert_eq!(request_configuration, configuration);
            reply
                .send(Loan {
                    owner: Some(owner),
                    epoch: "original-service-epoch".into(),
                    seq: 1,
                })
                .unwrap_or_else(|_| panic!("original owner receiver closed"));
        }
        _ => panic!("unexpected request"),
    }
    let owned = observed.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(
        Admission::take().is_err(),
        "one original finalizer slot stays charged"
    );
    Pending {
        owned,
        import_id,
        peer: Some(peer),
        release,
        worker,
        _serial: serial,
    }
}

#[test]
#[cfg(target_os = "linux")]
fn preclaim_refusals_return_the_exact_negative_and_release_original_connections() {
    let _serial = CLIENT_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for mode in [
        "ownerless",
        "shutdown",
        "full",
        "timeout",
        "stopped",
        "busy",
    ] {
        let f = Fixture::new();
        let import_id = id();
        let (reference, original) = transfer(staged(&f.location, &import_id));
        let negative = reference.not_committed(&f.location);
        let request = json!({"version":1,"stage":reference.stage,"import_id":reference.import_id,
            "record_sha256":reference.record_sha256,"max_bytes":reference.max_bytes,"directory":reference.directory});
        let hello: protocol::Hello = serde_json::from_value(json!({"abi":protocol::ABI,
            "configuration":f.location.configuration,"op":OP,"request":request}))
        .unwrap();
        let (peer, stream) = std::os::unix::net::UnixStream::pair().unwrap();
        let (send, receive) = mpsc::sync_channel(1);
        let _busy = if mode == "busy" {
            Some(Admission::take().unwrap())
        } else {
            None
        };
        if mode == "full" {
            send.try_send(super::super::super::Event::ListenerFailed)
                .unwrap_or_else(|_| panic!("fixture queue"));
        }
        let location = Location {
            root: f.location.root.clone(),
            configuration: f.location.configuration.clone(),
        };
        let worker = std::thread::spawn(move || {
            client(
                stream,
                hello,
                send,
                Arc::new(AtomicBool::new(mode == "stopped")),
                &location,
            )
        });
        let mut delayed = None;
        if matches!(mode, "ownerless" | "shutdown" | "timeout") {
            match receive.recv_timeout(Duration::from_secs(2)).unwrap() {
                super::super::super::Event::ImportOwner(_, reply) => {
                    if mode == "shutdown" {
                        drop(reply);
                    } else if mode == "timeout" {
                        delayed = Some(reply);
                    } else {
                        reply
                            .send(Loan {
                                owner: None,
                                epoch: "same-original-epoch".into(),
                                seq: 1,
                            })
                            .unwrap_or_else(|_| panic!("receiver closed"));
                    }
                }
                _ => panic!("unexpected request"),
            }
        }
        peer.set_read_timeout(Some(
            super::super::super::transport::TIMEOUT + Duration::from_secs(1),
        ))
        .unwrap();
        let bytes = protocol::line(&mut BufReader::new(peer), protocol::FRAME_BYTES).unwrap();
        worker.join().unwrap().unwrap();
        let response: Value = serde_json::from_slice(&bytes.unwrap()).unwrap();
        assert_eq!(response["value"], negative, "{mode}");
        if let Some(reply) = delayed {
            assert!(
                reply
                    .try_send(Loan {
                        owner: None,
                        epoch: "late-original-epoch".into(),
                        seq: 1
                    })
                    .is_err(),
                "expired admission never accepts a late loan"
            );
        }
        assert!(original.join("record-v1.json").is_file());
        assert!(!f.location.root.join("imports").join(import_id).exists());
        drop(_busy);
        assert!(Admission::take().is_ok());
    }
}

#[test]
#[cfg(target_os = "linux")]
fn postrename_failures_through_the_original_client_never_emit_a_negative_or_reserve() {
    use crate::desktop::notifications::preparation::{Preparation, TEST_SERIAL};
    let _preparing = TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _client = CLIENT_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for unknown_rename in [false, true] {
        let f = Fixture::new();
        let import = id();
        let (reference, original) = transfer(staged(&f.location, &import));
        let request = json!({"version":1,"stage":reference.stage,"import_id":reference.import_id,
            "record_sha256":reference.record_sha256,"max_bytes":reference.max_bytes,"directory":reference.directory});
        let hello = serde_json::from_value(json!({"abi":protocol::ABI,
            "configuration":f.location.configuration,"op":OP,"request":request}))
        .unwrap();
        // Acquire the real original service/handoff/admission owner. Join this
        // fixture's selection thread BEFORE assertions can panic; only its
        // original Owner loan remains, with no SDK/process/native delivery.
        let (release, wait) = mpsc::channel();
        let mut preparation =
            Preparation::blocked_for_test(Arc::new(f.location.lock().unwrap()), &f.location, wait);
        let owner = preparation.owner().unwrap();
        preparation.stop();
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(3);
        while !preparation.drained() {
            let _ = preparation.poll();
            assert!(Instant::now() < until);
            std::thread::yield_now();
        }
        drop(preparation);
        let location = Location {
            root: f.location.root.clone(),
            configuration: f.location.configuration.clone(),
        };
        let (peer, stream) = std::os::unix::net::UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let (send, receive) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            RENAME_ERROR.with(|enabled| enabled.set(unknown_rename));
            let result = client_with(
                stream,
                hello,
                send,
                Arc::new(AtomicBool::new(false)),
                &location,
                |owned| {
                    RENAMED.with(|saved| *saved.borrow_mut() = Some(owned.to_path_buf()));
                    anyhow::bail!("fixture after-claim failure")
                },
            );
            (result, RENAMED.with(|saved| saved.borrow_mut().take()))
        });
        match receive.recv_timeout(Duration::from_secs(2)).unwrap() {
            super::super::super::Event::ImportOwner(configuration, reply) => {
                assert_eq!(configuration, f.location.configuration);
                reply
                    .send(Loan {
                        owner: Some(owner),
                        epoch: "original-epoch".into(),
                        seq: 1,
                    })
                    .unwrap_or_else(|_| panic!("original receiver closed"));
            }
            _ => panic!("unexpected request"),
        }
        let (result, owned) = worker.join().unwrap();
        let frame = protocol::line(&mut BufReader::new(peer), protocol::FRAME_BYTES).unwrap();
        assert!(
            result.is_err(),
            "post-rename failure cannot settle negatively"
        );
        assert!(
            frame.is_none(),
            "neither success nor attempt-only negative is permitted"
        );
        let owned = owned.expect("the real rename completed");
        assert!(!original.exists());
        assert!(owned.join("record-v1.json").is_file());
        assert!(!f.location.root.join("imports").join(&import).exists());
        assert!(Admission::take().is_ok());
        assert!(f.location.handoff_lock().is_ok());
        assert!(f.location.lock().is_ok());
    }
}
