//! Original-object controls; native message acceptance is a separate layer.
use super::*;
use fs2::FileExt as _;
use std::{
    io::{Read, Seek},
    os::unix::net::UnixStream,
    process::{Command, Stdio},
};

const ENTRY: &str = "state::effort::picker_scope::driver::send::tests::fixture";
fn run(case: &str) {
    for kind in ["fresh", "moved"] {
        eprintln!("SEND_ORIGINAL_STARTED={kind}/{case}");
        let home = tempfile::tempdir().unwrap();
        let mut output = tempfile::tempfile().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", home.path())
            .env("HGS_CONFIG_DIR", home.path().join("config"))
            .env("HGS_STATE_DIR", home.path().join("state"))
            .env("TMPDIR", home.path())
            .env("ZERUS_SEND_CONTROL", case)
            .env("ZERUS_SEND_KIND", kind)
            .args(["--exact", ENTRY, "--ignored", "--test-threads=1"])
            .stdin(Stdio::null())
            .stdout(output.try_clone().unwrap())
            .stderr(output.try_clone().unwrap())
            .spawn()
            .unwrap();
        let end = Instant::now() + Duration::from_secs(12);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= end {
                let _ = child.kill();
                let _ = child.wait();
                panic!("Send fixture watchdog: {kind}/{case}");
            }
            std::thread::sleep(Duration::from_millis(3));
        };
        output.rewind().unwrap();
        let mut text = String::new();
        output.read_to_string(&mut text).unwrap();
        assert!(status.success(), "{kind}/{case}: {text}");
        eprintln!("SEND_ORIGINAL_PASSED={kind}/{case}");
    }
}
#[test]
fn pending_child_requires_exact_parent_marker_receipt_and_original_kind() {
    for case in [
        "marker",
        "outer-receipt",
        "child-receipt",
        "raw-cid",
        "kind",
        "record-version",
        "pending-object",
        "missing-child",
        "parent-uuid",
    ] {
        run(case);
    }
}
#[test]
fn submitted_receipt_needs_settled_enter_but_no_native_observation_turn() {
    for case in ["submitted", "no-enter", "expired-enter", "lost-endpoint"] {
        run(case);
    }
}
#[test]
fn send_observation_kinds_nonces_and_consumption_are_not_interchangeable() {
    for case in [
        "ready-after-paste",
        "settings-send",
        "foreign-nonce",
        "ended-child",
        "reused-turn",
    ] {
        run(case);
    }
}
#[test]
fn cache_replay_follows_existing_semantics_but_rejects_post_settlement_drift() {
    for case in [
        "cache-original",
        "cache-replaced",
        "cache-content",
        "cache-name",
        "cache-symlink",
        "cache-fifo",
    ] {
        run(case);
    }
}
#[test]
fn pending_capability_is_consumed_only_after_the_exact_applied_receipt_and_record() {
    for case in [
        "applied",
        "applied-status",
        "applied-model",
        "applied-marker",
        "applied-pending",
        "applied-kind",
        "applied-source",
    ] {
        run(case);
    }
}
#[test]
#[ignore = "bounded private Send original-object fixture"]
fn fixture() {
    let case = std::env::var("ZERUS_SEND_CONTROL").unwrap();
    let kind = std::env::var("ZERUS_SEND_KIND").unwrap();
    let (writer, mut record) = super::super::super::tests::setup(&kind);
    for name in ["input_receipts", "settings_receipts", "attachments"] {
        private_dir(&root().join(name)).unwrap();
    }
    let pending_id = uuid::Uuid::new_v4().to_string();
    let pending = json!({"request_id":pending_id,"model":"fixture-model","effort":"high","run_id":record["run_id"],"conversation_id":record["conversation_id"],"receipt_name":record["name"],"receipt_run_id":record["run_id"]});
    record["pending_settings"] = pending.clone();
    record["settings_unconfirmed_at"] = json!(123.0);
    atomic(
        &crate::state::storage::id_record_path(string(&record, "session_id")).unwrap(),
        &record.to_string(),
    )
    .unwrap();
    let raw=serde_json::to_vec(&json!({"request_id":uuid::Uuid::new_v4().to_string(),"expected_run_id":record["run_id"],"expected_conversation_id":record["conversation_id"],"text":"exact body"})).unwrap();
    let header = input::native_send::Header {
        dispatch_name: string(&record, "name").into(),
        agent: string(&record, "agent").into(),
        request_id: serde_json::from_slice::<Value>(&raw).unwrap()["request_id"]
            .as_str()
            .unwrap()
            .into(),
        run: string(&record, "run_id").into(),
        conversation: string(&record, "conversation_id").into(),
        compaction: String::new(),
        raw_len: raw.len(),
        digest: format!("{:x}", Sha256::digest(&raw)),
        pending_id: pending_id.clone(),
        pending_digest: input::native_send::pending_digest(&pending),
        machine: uuid::Uuid::new_v4().to_string(),
    };
    let value = input::native_send::Payload::parse(&raw).unwrap().receipt(
        &header,
        &record,
        &[],
        "exact body",
        now(),
    );
    let outer = root()
        .join("input_receipts")
        .join(format!("{}.json", header.request_id));
    atomic(&outer, &value.to_string()).unwrap();
    let child_value = json!({"request_id":pending_id,"status":"in_progress","model":"fixture-model","effort":"high","name":record["name"],"run_id":record["run_id"],"conversation_id":record["conversation_id"]});
    let child_path = root()
        .join("settings_receipts")
        .join(format!("{pending_id}.json"));
    atomic(&child_path, &child_value.to_string()).unwrap();
    let payload = input::native_send::Payload::parse(&raw).unwrap();
    let frame = host::frame::Frame::new_send(
        raw,
        input::native_send::Candidate {
            record: record.clone(),
            header,
            payload,
        },
        host::frame::Deadline::after(Duration::from_secs(8)).unwrap(),
    )
    .unwrap();
    let (peer, stream) = UnixStream::pair().unwrap();
    let control = host::Control::fixture(frame, stream).unwrap();
    let (owner, done) = owner::Owner::new(writer, control).unwrap();
    let entered = context::Entered::install(owner.clone()).unwrap();
    owner.open_scope(&record).unwrap();
    let (original, receipt, version, input_kind) = owner
        .with_scope(|scope| {
            Ok((
                Original::open(
                    &scope.receipts,
                    outer.file_name().unwrap().to_str().unwrap().into(),
                )?,
                Original::open(
                    &scope.send_directories.as_ref().unwrap().0,
                    format!("{pending_id}.json"),
                )?,
                scope.record.version.clone(),
                scope.proof.kind(),
            ))
        })
        .unwrap();
    let child = Child {
        nonce: uuid::Uuid::new_v4(),
        pending: pending.clone(),
        receipt,
        value: child_value,
        marker: json!(123.0),
        prior: None,
        version,
        kind: input_kind,
        closing: false,
    };
    let progress = Arc::new(Progress::default());
    *owner.send.lock().unwrap() = Some(Reservation {
        phase: Phase::Child,
        progress: progress.clone(),
        receipt: Some(original),
        value: value.clone(),
        pending,
        child: Some(child),
        pasted: b"exact body".to_vec(),
        marker: None,
    });
    let root_file = File::options()
        .read(true)
        .write(true)
        .open(root().join(".lock"))
        .unwrap();
    let sid = super::super::super::tests::input(&record);
    assert!(root_file.try_lock_exclusive().is_err() && sid.try_lock_exclusive().is_err());
    match case.as_str() {
        "marker" => {
            let mut changed = record.clone();
            changed["settings_unconfirmed_at"] = json!(124.0);
            assert!(own_marker(&owner, &changed).is_err());
        }
        "outer-receipt" => {
            fs::write(&outer, b"{}").unwrap();
            assert!(own_marker(&owner, &record).is_err());
        }
        "child-receipt" => {
            fs::write(&child_path, b"{}").unwrap();
            assert!(own_marker(&owner, &record).is_err());
        }
        "raw-cid" => {
            let mut changed = record.clone();
            changed["conversation_id"] = Value::Null;
            assert!(own_marker(&owner, &changed).is_err());
        }
        "kind" => {
            with(&owner, |held| {
                held.child.as_mut().unwrap().kind = match input_kind {
                    InputKind::Fresh => InputKind::LegacyMoved,
                    InputKind::LegacyMoved => InputKind::Fresh,
                };
                Ok(())
            })
            .unwrap();
            assert!(own_marker(&owner, &record).is_err());
        }
        "record-version" | "pending-object" => {
            with(&owner, |held| {
                let child = held.child.as_mut().unwrap();
                if case == "record-version" {
                    child.version = Value::Null;
                } else {
                    child.pending["model"] = json!("other");
                }
                Ok(())
            })
            .unwrap();
            assert!(own_marker(&owner, &record).is_err());
        }
        "ready-after-paste" | "settings-send" | "foreign-nonce" | "ended-child" | "reused-turn" => {
            let nonce = settings_nonce(&owner).unwrap();
            let (observed, consumed) = match case.as_str() {
                "ready-after-paste" => (context::Kind::SendReady, context::Kind::SendAfterPaste),
                "settings-send" => (
                    context::Kind::PendingSettings(nonce),
                    context::Kind::SendReady,
                ),
                "foreign-nonce" => (
                    context::Kind::PendingSettings(nonce),
                    context::Kind::PendingSettings(uuid::Uuid::new_v4()),
                ),
                "ended-child" => (
                    context::Kind::PendingSettings(nonce),
                    context::Kind::PendingSettings(nonce),
                ),
                _ => (context::Kind::SendAfterPaste, context::Kind::SendAfterPaste),
            };
            context::Observation::begin_kind(observed)
                .unwrap()
                .unwrap()
                .complete(&record)
                .unwrap();
            if case == "ended-child" {
                with(&owner, |held| {
                    held.child = None;
                    Ok(())
                })
                .unwrap();
                assert!(context::take_turn(&owner).is_err());
            } else if case == "reused-turn" {
                context::take_kind(&owner, consumed).unwrap();
                assert!(context::take_kind(&owner, consumed).is_err());
            } else {
                assert!(context::take_kind(&owner, consumed).is_err());
            }
        }
        "missing-child" => {
            with(&owner, |held| {
                held.child = None;
                Ok(())
            })
            .unwrap();
            assert!(settings_key_ready(&owner).is_err());
        }
        "parent-uuid" => {
            let request = Request {
                request_id: uuid::Uuid::new_v4().to_string(),
                expected_run_id: string(&record, "run_id").into(),
                expected_conversation_id: string(&record, "conversation_id").into(),
                effort: "high".into(),
            };
            assert_eq!(
                checked_settings(&owner, string(&record, "name"), &request).unwrap_err(),
                "Pending Settings observation belongs to another parent Send"
            );
        }
        "cache-original" | "cache-replaced" | "cache-content" | "cache-name" | "cache-symlink"
        | "cache-fifo" => {
            let h = super::header(&owner).unwrap();
            let mut submitted = value.clone();
            submitted["status"] = json!("submitted");
            if case == "cache-symlink" {
                submitted["opaque"] = json!("x".repeat(2 << 20));
                let target = root().join("private-large-cache.json");
                fs::write(&target, submitted.to_string()).unwrap();
                fs::remove_file(&outer).unwrap();
                std::os::unix::fs::symlink(target, &outer).unwrap();
            } else {
                fs::write(&outer, submitted.to_string()).unwrap();
            }
            let wire = input::native_send::cached(h).unwrap().unwrap();
            assert!(wire.len() + 8 <= input::native_send::REPLY_BYTES);
            match case.as_str() {
                "cache-original" | "cache-symlink" => {
                    assert_eq!(
                        input::native_send::decode(h, &wire, None).unwrap(),
                        submitted
                    );
                }
                "cache-name" => {
                    let mut renamed = h.clone();
                    renamed.dispatch_name.push_str("-renamed");
                    assert!(
                        input::native_send::decode(&renamed, &wire, None)
                            .unwrap_err()
                            .starts_with("delivery uncertain")
                    );
                }
                _ => {
                    if case == "cache-replaced" {
                        let replacement = root().join("replaced-cache.json");
                        fs::write(&replacement, submitted.to_string()).unwrap();
                        fs::rename(replacement, &outer).unwrap();
                    } else if case == "cache-content" {
                        submitted["opaque"] = json!("replacement");
                        fs::write(&outer, submitted.to_string()).unwrap();
                    } else {
                        fs::remove_file(&outer).unwrap();
                        let path =
                            std::ffi::CString::new(outer.as_os_str().as_encoded_bytes()).unwrap();
                        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
                    }
                    let error = input::native_send::decode(h, &wire, None).unwrap_err();
                    assert!(error.starts_with("delivery uncertain"), "{error}");
                    assert_eq!(error.matches("delivery uncertain").count(), 1);
                }
            }
        }
        "applied" | "applied-status" | "applied-model" | "applied-marker" | "applied-pending"
        | "applied-kind" | "applied-source" => {
            let mut completed = record.clone();
            completed
                .as_object_mut()
                .unwrap()
                .remove("pending_settings");
            completed
                .as_object_mut()
                .unwrap()
                .remove("settings_unconfirmed_at");
            completed["model"] = json!("fixture-model");
            completed["effort"] = json!("high");
            if case == "applied-model" {
                completed["model"] = json!("other");
            }
            if case == "applied-marker" {
                completed["settings_unconfirmed_at"] = json!(999.0);
            }
            if case == "applied-pending" {
                completed["pending_settings"] = record["pending_settings"].clone();
            }
            let (applied, mut final_value, version) = owner
                .with_scope(|scope| {
                    // Controlled before-images in this private fixture. Production
                    // obtains the same objects only from the guarded native commit.
                    fs::write(
                        scope.sessions.path.join(&scope.record.name),
                        completed.to_string(),
                    )
                    .map_err(|e| e.to_string())?;
                    scope.record = Original::open(&scope.sessions, scope.record.name.clone())?;
                    let mut value = serde_json::from_slice::<Value>(
                        &fs::read(&child_path).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?;
                    value["status"] = json!(if case == "applied-status" {
                        "in_progress"
                    } else {
                        "applied"
                    });
                    fs::write(&child_path, value.to_string()).map_err(|e| e.to_string())?;
                    let applied = Original::open(
                        &scope.send_directories.as_ref().unwrap().0,
                        child_path.file_name().unwrap().to_str().unwrap().into(),
                    )?;
                    Ok((applied, value, scope.record.version.clone()))
                })
                .unwrap();
            with(&owner, |held| {
                let child = held.child.as_mut().unwrap();
                child.closing = true;
                child.version = version;
                if case == "applied-kind" {
                    child.kind = match child.kind {
                        InputKind::Fresh => InputKind::LegacyMoved,
                        InputKind::LegacyMoved => InputKind::Fresh,
                    };
                }
                Ok(())
            })
            .unwrap();
            if case == "applied-source" {
                final_value["status"] = json!("applied");
                fs::write(&child_path, b"{}").unwrap();
            }
            let result = finish_pending(&owner, applied, final_value);
            if case == "applied" {
                result.unwrap();
                assert!(owner.send.lock().unwrap().as_ref().unwrap().child.is_none());
            } else {
                assert!(result.is_err(), "{case}");
                assert!(owner.send.lock().unwrap().as_ref().unwrap().child.is_some());
            }
        }
        "submitted" | "no-enter" | "expired-enter" | "lost-endpoint" => {
            with(&owner, |held| {
                held.child = None;
                held.phase = Phase::Entered;
                Ok(())
            })
            .unwrap();
            progress
                .enter_settled
                .store(case != "no-enter", Ordering::Release);
            let mut submitted = value.clone();
            submitted["status"] = json!("submitted");
            let staged = owner
                .with_scope(|scope| {
                    publication::Staged::prepare(
                        &scope.receipts,
                        outer.file_name().unwrap().to_str().unwrap(),
                        submitted.to_string().as_bytes(),
                    )
                })
                .unwrap();
            if case == "lost-endpoint" {
                peer.shutdown(std::net::Shutdown::Both).unwrap();
            }
            if case == "expired-enter" {
                std::thread::sleep(
                    owner.until.saturating_duration_since(Instant::now())
                        + Duration::from_millis(5),
                );
            }
            let witness = Publication::new(
                &owner,
                &staged,
                outer.parent().unwrap().into(),
                outer.file_name().unwrap().to_str().unwrap().into(),
                true,
                None,
            );
            if case == "submitted" {
                let fence =
                    publication::CommitFence::send(&owner, &staged, witness.unwrap()).unwrap();
                staged.commit(&fence).unwrap();
                assert_eq!(
                    serde_json::from_slice::<Value>(&fs::read(&outer).unwrap()).unwrap()["status"],
                    "submitted"
                );
            } else {
                assert!(witness.is_err());
                assert_eq!(
                    serde_json::from_slice::<Value>(&fs::read(&outer).unwrap()).unwrap()["status"],
                    "in_progress"
                );
            }
        }
        _ => panic!("unknown Send fixture"),
    }
    assert!(root_file.try_lock_exclusive().is_err() && sid.try_lock_exclusive().is_err());
    drop(entered);
    drop(owner);
    done.recv_timeout(Duration::from_secs(2)).unwrap();
    root_file.try_lock_exclusive().unwrap();
    sid.try_lock_exclusive().unwrap();
}
