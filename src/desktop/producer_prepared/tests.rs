use super::*;
use zerus_client_core::desktop::{Build, ABI};
use zerus_desktop_migration::canonical;

const REQUEST: &str = "11111111-1111-4111-8111-111111111111";
const PRODUCER: &str = "22222222-2222-4222-8222-222222222222";
fn fixture() -> (tempfile::TempDir, Location, Store, String) {
    let temp = tempfile::tempdir().unwrap();
    let location = Location {
        root: canonical(&temp.path().join("state/desktop")).unwrap(),
        configuration: "a".repeat(64),
    };
    location.prepare().unwrap();
    let epoch = uuid::Uuid::new_v4().to_string();
    let store = Store::open(&location, &epoch).unwrap();
    (temp, location, store, epoch)
}
fn binding(epoch: &str) -> Binding {
    Binding {
        version: 1,
        request_id: REQUEST.into(),
        configuration: "a".repeat(64),
        service_epoch: epoch.into(),
        producer_epoch: PRODUCER.into(),
        frontend: Hello {
            abi: ABI.into(),
            version: 1,
            nonce: "33333333-3333-4333-8333-333333333333".into(),
            application: "com.hgdev.hgs-tray".into(),
            gui: "tauri".into(),
            build: Build {
                version: "1.2.3".into(),
                source_commit: "b".repeat(40),
                source_dirty: Some(false),
            },
            instance_epoch: "44444444-4444-4444-8444-444444444444".into(),
            writer_epoch: None,
            configuration: Some("a".repeat(64)),
            live: true,
            replay: false,
        },
        writer_epoch: uuid::Uuid::new_v4().to_string(),
        drain_id: uuid::Uuid::new_v4().to_string(),
        import_id: uuid::Uuid::new_v4().to_string(),
        profile_id: BACKGROUND_V1.id().into(),
        profile_hash: BACKGROUND_V1.hash(),
    }
}
fn ticket(saved: Saved) -> Ticket {
    match saved {
        Saved::Created(ticket) => ticket,
        _ => panic!("expected original commit"),
    }
}
fn bytes(store: &Store) -> String {
    store
        .db
        .query_row(
            "SELECT binding FROM desktop_producer_prepared WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn preparation_is_immutable_and_duplicate_delivery_never_mints_another_mutation_key() {
    let (_temp, location, mut store, epoch) = fixture();
    let presence = super::super::presence_admission::fence(&location).unwrap();
    let lock = location.handoff_lock().unwrap();
    assert!(inspect_db(&store.db, &location.configuration)
        .unwrap()
        .is_none());
    let original = ticket(save(&location, &lock, &presence, &mut store, binding(&epoch)).unwrap());
    let before = bytes(&store);
    // A fresh random candidate cannot replace the original writer/drain/import IDs.
    let Saved::Existing(snapshot) =
        save(&location, &lock, &presence, &mut store, binding(&epoch)).unwrap()
    else {
        panic!("duplicate granted mutation key")
    };
    assert_eq!(snapshot.request_id(), original.request_id);
    assert_eq!(snapshot.digest, original.digest);
    assert_eq!(snapshot.phase(), Phase::Prepared);
    assert_eq!(bytes(&store), before);
    for changed in 0..3 {
        let mut candidate = binding(&epoch);
        match changed {
            0 => candidate.request_id = uuid::Uuid::new_v4().to_string(),
            1 => candidate.producer_epoch = uuid::Uuid::new_v4().to_string(),
            _ => candidate.frontend.build.source_commit = "c".repeat(40),
        }
        assert!(save(&location, &lock, &presence, &mut store, candidate).is_err());
        assert_eq!(bytes(&store), before);
    }
}

#[test]
fn dropped_result_and_service_restart_retain_the_original_fence_and_legacy_refusal() {
    let (_temp, location, mut store, epoch) = fixture();
    let presence = super::super::presence_admission::fence(&location).unwrap();
    let lock = location.handoff_lock().unwrap();
    drop(save(&location, &lock, &presence, &mut store, binding(&epoch)).unwrap());
    let before = bytes(&store);
    assert!(super::super::store::writer_active(&location).is_err());
    // Ordinary writer revocation cannot erase an unsettled preparation.
    zerus_desktop_ledger::revoke(&store.db, "manual_revoke").unwrap();
    assert!(super::super::store::writer_active(&location).is_err());
    drop(store);
    let new_epoch = uuid::Uuid::new_v4().to_string();
    let mut store = Store::open(&location, &new_epoch).unwrap();
    let snapshot = inspect_db(&store.db, &location.configuration)
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.binding.service_epoch, epoch);
    assert_eq!(snapshot.phase(), Phase::Prepared);
    assert_eq!(bytes(&store), before);
    assert!(save(&location, &lock, &presence, &mut store, binding(&new_epoch)).is_err());
    assert_eq!(bytes(&store), before);
}

#[test]
fn abort_is_only_negative_reconciliation_intent_and_never_reopens_writers() {
    let (_temp, location, mut store, epoch) = fixture();
    let presence = super::super::presence_admission::fence(&location).unwrap();
    let lock = location.handoff_lock().unwrap();
    let original = ticket(save(&location, &lock, &presence, &mut store, binding(&epoch)).unwrap());
    let before = bytes(&store);
    let bad = Ticket {
        request_id: original.request_id.clone(),
        digest: "f".repeat(64),
    };
    assert!(request_abort(&location, &lock, &presence, &mut store, &bad).is_err());
    for _ in 0..2 {
        let snapshot = request_abort(&location, &lock, &presence, &mut store, &original).unwrap();
        assert_eq!(snapshot.phase(), Phase::AbortRequested);
        assert_eq!(snapshot.digest, original.digest);
        assert_eq!(bytes(&store), before);
        assert!(super::super::store::writer_active(&location).is_err());
    }
    drop(store);
    let mut store = Store::open(&location, &uuid::Uuid::new_v4().to_string()).unwrap();
    assert_eq!(
        inspect_db(&store.db, &location.configuration)
            .unwrap()
            .unwrap()
            .phase(),
        Phase::AbortRequested
    );
    assert!(request_abort(&location, &lock, &presence, &mut store, &original).is_err());
    assert!(super::super::store::writer_active(&location).is_err());
}

// Complete synthetic fixture state, including schema and every table/column.
// This is deliberately broader than the prepared journal mutation under test.
fn database_snapshot(
    db: &Connection,
) -> std::collections::BTreeMap<String, Vec<Vec<rusqlite::types::Value>>> {
    let mut names: Vec<String> = db
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    names.push("sqlite_schema".into());
    names
        .into_iter()
        .map(|name| {
            let mut stmt = db
                .prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\"")))
                .unwrap();
            let columns = stmt.column_count();
            let rows = stmt
                .query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            (name, rows)
        })
        .collect()
}

#[test]
fn persisted_abort_after_restart_changes_only_the_original_phase_and_has_no_follow_up() {
    let (_temp, location, mut store, epoch) = fixture();
    let presence = super::super::presence_admission::fence(&location).unwrap();
    let lock = location.handoff_lock().unwrap();
    drop(save(&location, &lock, &presence, &mut store, binding(&epoch)).unwrap());
    drop(store); // No Ticket or original observation survives the old process.
    let current_epoch = uuid::Uuid::new_v4().to_string();
    let mut store = Store::open(&location, &current_epoch).unwrap();
    store.db.execute_batch("CREATE TABLE recovery_sentinel (id INTEGER PRIMARY KEY, value BLOB); INSERT INTO recovery_sentinel VALUES(1,X'00FF');").unwrap();
    let original = inspect_db(&store.db, &location.configuration)
        .unwrap()
        .unwrap();
    let mut expected = database_snapshot(&store.db);
    expected.get_mut("desktop_producer_prepared").unwrap()[0][3] =
        "abort_requested".to_owned().into();
    for _ in 0..2 {
        let result = request_abort_original(
            &location,
            &lock,
            &presence,
            &mut store,
            &original,
            &current_epoch,
        )
        .unwrap();
        assert_eq!(result.phase(), Phase::AbortRequested);
        assert_eq!(result.digest, original.digest);
        assert_eq!(result.binding.service_epoch, epoch);
        assert_eq!(database_snapshot(&store.db), expected);
        assert!(super::super::store::writer_active(&location).is_err());
        assert!(super::super::startup_exclusion::Startup::hold(&location).is_err());
    }
    drop(store);
    let next_epoch = uuid::Uuid::new_v4().to_string();
    let mut store = Store::open(&location, &next_epoch).unwrap();
    let before = database_snapshot(&store.db);
    assert!(request_abort_original(
        &location,
        &lock,
        &presence,
        &mut store,
        &original,
        &current_epoch
    )
    .is_err());
    request_abort_original(
        &location,
        &lock,
        &presence,
        &mut store,
        &original,
        &next_epoch,
    )
    .unwrap();
    assert_eq!(database_snapshot(&store.db), before);
    assert!(super::super::store::writer_active(&location).is_err());
}

#[test]
fn recovered_abort_refuses_wrong_locks_presence_and_current_epoch_without_any_write() {
    let (_temp, location, mut store, epoch) = fixture();
    let (_other_temp, other, _other_store, _) = fixture();
    let presence = super::super::presence_admission::fence(&location).unwrap();
    let other_presence = super::super::presence_admission::fence(&other).unwrap();
    let lock = location.handoff_lock().unwrap();
    let other_lock = other.handoff_lock().unwrap();
    drop(save(&location, &lock, &presence, &mut store, binding(&epoch)).unwrap());
    let original = inspect_db(&store.db, &location.configuration)
        .unwrap()
        .unwrap();
    let before = database_snapshot(&store.db);
    assert!(request_abort_original(
        &location,
        &other_lock,
        &presence,
        &mut store,
        &original,
        &epoch
    )
    .is_err());
    assert!(request_abort_original(
        &location,
        &lock,
        &other_presence,
        &mut store,
        &original,
        &epoch
    )
    .is_err());
    for wrong in [
        "".to_owned(),
        uuid::Uuid::nil().to_string(),
        uuid::Uuid::new_v4().to_string(),
    ] {
        assert!(
            request_abort_original(&location, &lock, &presence, &mut store, &original, &wrong)
                .is_err()
        );
    }
    assert_eq!(database_snapshot(&store.db), before);
    request_abort_original(&location, &lock, &presence, &mut store, &original, &epoch).unwrap();
    let before = database_snapshot(&store.db);
    // Even an already-aborted original must pass the independent current check.
    assert!(request_abort_original(
        &location,
        &lock,
        &presence,
        &mut store,
        &original,
        &uuid::Uuid::new_v4().to_string()
    )
    .is_err());
    assert_eq!(database_snapshot(&store.db), before);
}

#[test]
fn recovered_abort_refuses_foreign_or_changed_original_without_any_write() {
    for change in 0..5 {
        let (_temp, location, mut store, epoch) = fixture();
        let presence = super::super::presence_admission::fence(&location).unwrap();
        let lock = location.handoff_lock().unwrap();
        drop(save(&location, &lock, &presence, &mut store, binding(&epoch)).unwrap());
        let mut original = inspect_db(&store.db, &location.configuration)
            .unwrap()
            .unwrap();
        match change {
            0 => original.binding.request_id = uuid::Uuid::new_v4().to_string(),
            1 => original.digest = "f".repeat(64),
            2 => original.binding.configuration = "b".repeat(64),
            3 => {
                store
                    .db
                    .execute("UPDATE service SET configuration=?1", ["b".repeat(64)])
                    .unwrap();
            }
            _ => {
                let mut changed = binding(&epoch);
                changed.frontend.instance_epoch = uuid::Uuid::new_v4().to_string();
                let bytes = serde_json::to_vec(&changed).unwrap();
                store
                    .db
                    .execute(
                        "UPDATE desktop_producer_prepared SET binding=?1,digest=?2",
                        params![std::str::from_utf8(&bytes).unwrap(), digest(&bytes)],
                    )
                    .unwrap();
            }
        }
        let before = database_snapshot(&store.db);
        assert!(
            request_abort_original(&location, &lock, &presence, &mut store, &original, &epoch)
                .is_err()
        );
        assert_eq!(database_snapshot(&store.db), before);
        assert!(super::super::store::writer_active(&location).is_err());
    }
}

#[test]
fn recovered_abort_refuses_missing_malformed_and_unavailable_journal_without_any_write() {
    for change in 0..5 {
        let (_temp, location, mut store, epoch) = fixture();
        let presence = super::super::presence_admission::fence(&location).unwrap();
        let lock = location.handoff_lock().unwrap();
        drop(save(&location, &lock, &presence, &mut store, binding(&epoch)).unwrap());
        let original = inspect_db(&store.db, &location.configuration)
            .unwrap()
            .unwrap();
        match change {
            0 => store.db.execute_batch("DELETE FROM desktop_producer_prepared").unwrap(),
            1 => store.db.execute_batch("UPDATE desktop_producer_prepared SET digest='invalid'").unwrap(),
            2 => store.db.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE desktop_producer_prepared SET phase='complete'").unwrap(),
            3 => store.db.execute_batch("ALTER TABLE desktop_producer_prepared RENAME TO hidden_prepared; CREATE VIEW desktop_producer_prepared AS SELECT * FROM hidden_prepared").unwrap(),
            _ => store.db.execute_batch("DROP TABLE desktop_producer_prepared").unwrap(),
        }
        let before = database_snapshot(&store.db);
        assert!(
            request_abort_original(&location, &lock, &presence, &mut store, &original, &epoch)
                .is_err()
        );
        assert_eq!(database_snapshot(&store.db), before);
    }
}

#[test]
fn wrong_lock_presence_configuration_and_epoch_refuse_before_commit() {
    let (_temp, location, mut store, epoch) = fixture();
    let (_other_temp, other, _other_store, _) = fixture();
    let presence = super::super::presence_admission::fence(&location).unwrap();
    let other_presence = super::super::presence_admission::fence(&other).unwrap();
    let lock = location.handoff_lock().unwrap();
    let other_lock = other.handoff_lock().unwrap();
    assert!(save(
        &location,
        &other_lock,
        &presence,
        &mut store,
        binding(&epoch)
    )
    .is_err());
    assert!(save(
        &location,
        &lock,
        &other_presence,
        &mut store,
        binding(&epoch)
    )
    .is_err());
    assert!(save(
        &location,
        &lock,
        &presence,
        &mut store,
        binding(&uuid::Uuid::new_v4().to_string())
    )
    .is_err());
    let mut candidate = binding(&epoch);
    candidate.configuration = "c".repeat(64);
    assert!(save(&location, &lock, &presence, &mut store, candidate).is_err());
    assert!(inspect_db(&store.db, &location.configuration)
        .unwrap()
        .is_none());
}

#[test]
fn corrupt_changed_oversized_unknown_or_extra_rows_never_look_absent() {
    for changed in 0..9 {
        let (_temp, location, mut store, epoch) = fixture();
        let presence = super::super::presence_admission::fence(&location).unwrap();
        let lock = location.handoff_lock().unwrap();
        save(&location, &lock, &presence, &mut store, binding(&epoch)).unwrap();
        let original = bytes(&store);
        match changed {
            0 => {
                store
                    .db
                    .execute(
                        "UPDATE desktop_producer_prepared SET digest=?1",
                        ["f".repeat(64)],
                    )
                    .unwrap();
            }
            1 => {
                store
                    .db
                    .execute(
                        "UPDATE desktop_producer_prepared SET binding=?1",
                        ["x".repeat(BYTES + 1)],
                    )
                    .unwrap();
            }
            2..=4 => {
                let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
                match changed {
                    2 => value["version"] = 2.into(),
                    3 => value["configuration"] = "c".repeat(64).into(),
                    _ => value["authority"] = true.into(),
                }
                let body = serde_json::to_string(&value).unwrap();
                store
                    .db
                    .execute(
                        "UPDATE desktop_producer_prepared SET binding=?1,digest=?2",
                        params![body, digest(body.as_bytes())],
                    )
                    .unwrap();
            }
            5 => {
                store.db.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE desktop_producer_prepared SET phase='complete';").unwrap();
            }
            6 => {
                store.db.execute_batch("PRAGMA ignore_check_constraints=ON; INSERT INTO desktop_producer_prepared SELECT 2,binding,digest,phase FROM desktop_producer_prepared;").unwrap();
            }
            7 => {
                store.db.execute_batch("ALTER TABLE desktop_producer_prepared RENAME TO old_prepared; CREATE VIEW desktop_producer_prepared AS SELECT * FROM old_prepared WHERE 0;").unwrap();
            }
            _ => {
                store
                    .db
                    .execute_batch("DROP TABLE desktop_producer_prepared")
                    .unwrap();
            }
        }
        assert!(inspect_db(&store.db, &location.configuration).is_err());
        assert!(super::super::store::writer_active(&location).is_err());
    }
}

#[test]
fn a_downgraded_schema_marker_never_hides_a_retained_preparation() {
    let (_temp, location, mut store, epoch) = fixture();
    let presence = super::super::presence_admission::fence(&location).unwrap();
    let lock = location.handoff_lock().unwrap();
    save(&location, &lock, &presence, &mut store, binding(&epoch)).unwrap();
    for version in [1, 8, 999] {
        store
            .db
            .execute_batch(&format!("PRAGMA user_version={version}"))
            .unwrap();
        assert!(super::super::store::writer_active(&location).is_err());
    }
}

#[test]
fn invalid_current_frontend_or_profile_cannot_create_a_preparation() {
    for changed in 0..7 {
        let (_temp, location, mut store, epoch) = fixture();
        let presence = super::super::presence_admission::fence(&location).unwrap();
        let lock = location.handoff_lock().unwrap();
        let mut candidate = binding(&epoch);
        match changed {
            0 => candidate.frontend.build.source_dirty = Some(true),
            1 => candidate.frontend.replay = true,
            2 => candidate.frontend.writer_epoch = Some(epoch),
            3 => candidate.profile_hash = "f".repeat(64),
            4 => candidate.request_id = "00000000-0000-0000-0000-000000000000".into(),
            5 => candidate.import_id = "invalid".into(),
            _ => candidate.frontend.instance_epoch = "00000000-0000-0000-0000-000000000000".into(),
        }
        assert!(save(&location, &lock, &presence, &mut store, candidate).is_err());
        assert!(inspect_db(&store.db, &location.configuration)
            .unwrap()
            .is_none());
    }
}

#[test]
fn child_fixture_commits_without_running_destructors() {
    let Some(path) = std::env::var_os("ZERUS_PREPARED_CRASH_FIXTURE") else {
        return;
    };
    let location = Location {
        root: path.into(),
        configuration: "a".repeat(64),
    };
    let epoch = uuid::Uuid::new_v4().to_string();
    let mut store = Store::open(&location, &epoch).unwrap();
    let _owner = location.lock().unwrap();
    let presence = super::super::presence_admission::fence(&location).unwrap();
    let lock = location.handoff_lock().unwrap();
    save(&location, &lock, &presence, &mut store, binding(&epoch)).unwrap();
    use std::io::Write;
    println!("PREPARED_FIXTURE_COMMITTED {REQUEST}");
    std::io::stdout().flush().unwrap();
    std::process::exit(86);
}

#[test]
fn committed_preparation_survives_an_actual_process_exit_without_reply_or_drop() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let (temp, location, store, _) = fixture();
    drop(store);
    let private = temp.path().join("child");
    for name in [
        "home", "tmp", "tmux", "config", "state", "data", "cache", "runtime",
    ] {
        crate::platform::private_dir(&private.join(name)).unwrap();
    }
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "desktop::producer_prepared::tests::child_fixture_commits_without_running_destructors",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", private.join("home"))
        .env("TMPDIR", private.join("tmp"))
        .env("TMUX_TMPDIR", private.join("tmux"))
        .env("HGS_CONFIG_DIR", private.join("config"))
        .env("HGS_STATE_DIR", private.join("state"))
        .env("XDG_CONFIG_HOME", private.join("config"))
        .env("XDG_STATE_HOME", private.join("state"))
        .env("XDG_DATA_HOME", private.join("data"))
        .env("XDG_CACHE_HOME", private.join("cache"))
        .env("XDG_RUNTIME_DIR", private.join("runtime"))
        .env("ZERUS_PREPARED_CRASH_FIXTURE", &location.root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = crate::platform::spawn_in_turn(&mut command).unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= end {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("private crash fixture exceeded its deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(86),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains(&format!("PREPARED_FIXTURE_COMMITTED {REQUEST}")));
    assert!(super::super::store::writer_active(&location).is_err());
    let current_epoch = uuid::Uuid::new_v4().to_string();
    let mut store = Store::open(&location, &current_epoch).unwrap();
    let snapshot = inspect_db(&store.db, &location.configuration)
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.request_id(), REQUEST);
    assert_eq!(snapshot.phase(), Phase::Prepared);
    let presence = super::super::presence_admission::fence(&location).unwrap();
    let lock = location.handoff_lock().unwrap();
    let original_bytes = bytes(&store);
    let result = request_abort_original(
        &location,
        &lock,
        &presence,
        &mut store,
        &snapshot,
        &current_epoch,
    )
    .unwrap();
    assert_eq!(result.phase(), Phase::AbortRequested);
    assert_eq!(bytes(&store), original_bytes);
    assert!(super::super::store::writer_active(&location).is_err());
    assert!(super::super::startup_exclusion::Startup::hold(&location).is_err());
}

// Every probe uses a real private Unix listener. The original channel remains
// open in the returned fixture; replacing only the canonical reply cannot
// manufacture a new association on that channel.
struct Bound {
    registry: super::super::producer_session::Registry,
    session: Session,
    peer: std::os::unix::net::UnixStream,
    listener: std::os::unix::net::UnixListener,
    responder: std::sync::Arc<zerus_client_core::desktop::Responder>,
}
fn configure_probe_io(stream: &std::os::unix::net::UnixStream) {
    use std::time::Duration;
    // BSD accepts may inherit O_NONBLOCK. Timeouts only bound blocking I/O.
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(1)))
        .unwrap();
}

#[test]
fn accepted_canonical_probe_normalizes_nonblocking_before_bounded_reads() {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        os::{
            fd::AsRawFd,
            unix::net::{UnixListener, UnixStream},
        },
        time::Duration,
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("canonical");
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut peer = UnixStream::connect(&path).unwrap();
    let (mut stream, _) = listener.accept().unwrap();
    // Force the inherited state on every Unix platform, including Linux.
    // With no query written yet, the old timeout-only setup returns WouldBlock.
    stream.set_nonblocking(true).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(
        stream.read(&mut [0]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    configure_probe_io(&stream);
    let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0, "accepted stream flags unavailable");
    assert_eq!(
        flags & libc::O_NONBLOCK,
        0,
        "accepted probe must use blocking timeout I/O"
    );
    let listener_flags = unsafe { libc::fcntl(listener.as_raw_fd(), libc::F_GETFL) };
    assert!(listener_flags >= 0, "listener flags unavailable");
    assert_ne!(
        listener_flags & libc::O_NONBLOCK,
        0,
        "accept remains nonblocking"
    );
    assert_eq!(stream.read_timeout().unwrap(), Some(Duration::from_secs(1)));
    assert_eq!(
        stream.write_timeout().unwrap(),
        Some(Duration::from_secs(1))
    );
    let query = zerus_client_core::desktop::query(REQUEST).unwrap();
    peer.write_all(&query).unwrap();
    let mut received = Vec::new();
    BufReader::new(&mut stream)
        .take(64)
        .read_until(b'\n', &mut received)
        .unwrap();
    assert_eq!(received, query);
}

fn answer_probe(
    listener: &std::os::unix::net::UnixListener,
    responder: &zerus_client_core::desktop::Responder,
    changed: usize,
) {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        time::{Duration, Instant},
    };
    let end = Instant::now() + Duration::from_secs(3);
    let mut canonical = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < end => {
                std::thread::sleep(Duration::from_millis(2));
            }
            result => panic!("private canonical accept {result:?}"),
        }
    };
    configure_probe_io(&canonical);
    let mut query = Vec::new();
    BufReader::new(&mut canonical)
        .take(64)
        .read_until(b'\n', &mut query)
        .unwrap();
    let reply = responder.reply(query.strip_suffix(b"\n").unwrap()).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&reply).unwrap();
    match changed {
        0 => (),
        1 => value["instance_epoch"] = uuid::Uuid::new_v4().to_string().into(),
        2 => value["build"]["source_commit"] = "c".repeat(40).into(),
        3 => value["configuration"] = "c".repeat(64).into(),
        4 => value["replay"] = true.into(),
        5 => value["writer_epoch"] = uuid::Uuid::new_v4().to_string().into(),
        _ => unreachable!(),
    }
    writeln!(canonical, "{value}").unwrap();
}
fn bound(location: &Location, epoch: &str, path: &std::path::Path) -> Bound {
    use super::super::producer_session::Registry;
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::{UnixListener, UnixStream},
        sync::Arc,
        time::{Duration, Instant},
    };
    use zerus_client_core::desktop::{self, producer::Challenge, Responder};
    let source = binding(epoch);
    let responder = Arc::new(Responder::new(
        source.frontend.application.clone(),
        source.frontend.build.clone(),
        source.frontend.instance_epoch.clone(),
        true,
        false,
    ));
    assert!(responder.configure_once(location.configuration.clone()));
    let query = desktop::query(&source.frontend.nonce).unwrap();
    let expected =
        serde_json::from_slice(&responder.reply(&query[..query.len() - 1]).unwrap()).unwrap();
    let listener = UnixListener::bind(path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let registry = Registry::new(location.configuration.clone(), epoch.into()).unwrap();
    let (stream, mut peer) = UnixStream::pair().unwrap();
    // XNU may reject setsockopt after the other endpoint has closed: configure
    // the timeout before handing over the channel, never after refusal.
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    peer.set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let pending = registry.begin(BufReader::new(stream), expected).unwrap();
    let frontend = std::thread::spawn(move || {
        let mut offer = String::new();
        BufReader::new(&mut peer).read_line(&mut offer).unwrap();
        let offer: serde_json::Value = serde_json::from_str(&offer).unwrap();
        let challenge: Challenge = serde_json::from_value(offer["challenge"].clone()).unwrap();
        assert!(responder.install_challenge(challenge, Instant::now()));
        let ready = serde_json::json!({"abi":offer["abi"],"version":1,"op":"ready","sequence":1,"configuration":offer["configuration"],"instance_epoch":offer["expected"]["instance_epoch"],"challenge":offer["challenge"]});
        writeln!(peer, "{ready}").unwrap();
        answer_probe(&listener, &responder, 0);
        (peer, listener, responder)
    });
    let session = pending.bind(path).unwrap();
    let (peer, listener, responder) = frontend.join().unwrap();
    Bound {
        registry,
        session,
        peer,
        listener,
        responder,
    }
}
fn assert_ended(
    registry: &super::super::producer_session::Registry,
    peer: &mut std::os::unix::net::UnixStream,
    epoch: &str,
) {
    use std::{
        io::{BufReader, Read},
        os::unix::net::UnixStream,
    };
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    let (stream, _peer) = UnixStream::pair().unwrap();
    // Only the real channel's destruction may have released this exact slot.
    let pending = registry
        .begin(BufReader::new(stream), binding(epoch).frontend)
        .unwrap();
    drop(pending);
}

#[test]
fn original_session_is_retained_while_its_exact_binding_is_durably_prepared() {
    let (temp, location, mut store, epoch) = fixture();
    let path = temp.path().join("canonical");
    let Bound {
        registry,
        session,
        mut peer,
        listener,
        responder,
    } = bound(&location, &epoch, &path);
    let frontend = std::thread::spawn(move || answer_probe(&listener, &responder, 0));
    let context = Context::at(&location, session, &path).unwrap();
    frontend.join().unwrap();
    assert!(super::super::startup_exclusion::Startup::hold(&location).is_err());
    let _ticket = ticket(prepare(&location, &mut store, &context, REQUEST).unwrap());
    let snapshot = inspect_db(&store.db, &location.configuration)
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot.binding.frontend,
        *context.session.identity().hello()
    );
    assert_eq!(
        snapshot.binding.producer_epoch,
        context.session.identity().challenge().producer_epoch
    );
    drop(context);
    assert_ended(&registry, &mut peer, &epoch);
    assert!(super::super::store::writer_active(&location).is_err());
    assert!(super::super::startup_exclusion::Startup::hold(&location).is_err());
}

#[test]
fn replacement_identity_or_changed_canonical_state_consumes_the_stale_session_without_preparing() {
    for changed in 1..=5 {
        let (temp, location, store, epoch) = fixture();
        let path = temp.path().join("canonical");
        let Bound {
            registry,
            session,
            mut peer,
            listener,
            responder,
        } = bound(&location, &epoch, &path);
        let frontend = std::thread::spawn(move || answer_probe(&listener, &responder, changed));
        assert!(Context::at(&location, session, &path).is_err());
        frontend.join().unwrap();
        assert_ended(&registry, &mut peer, &epoch);
        assert!(inspect_db(&store.db, &location.configuration)
            .unwrap()
            .is_none());
        assert!(super::super::startup_exclusion::Startup::hold(&location).is_ok());
        assert!(super::super::presence_admission::fence(&location).is_ok());
    }
}

#[test]
fn missing_canonical_endpoint_is_not_a_preparation_and_releases_only_its_owned_context() {
    let (temp, location, store, epoch) = fixture();
    let path = temp.path().join("canonical");
    let Bound {
        registry,
        session,
        mut peer,
        listener,
        responder: _,
    } = bound(&location, &epoch, &path);
    drop(listener);
    std::fs::remove_file(&path).unwrap();
    assert!(Context::at(&location, session, &path).is_err());
    assert_ended(&registry, &mut peer, &epoch);
    assert!(inspect_db(&store.db, &location.configuration)
        .unwrap()
        .is_none());
    assert!(super::super::startup_exclusion::Startup::hold(&location).is_ok());
}

#[test]
fn replacement_startup_holds_the_boundary_before_any_preparer_canonical_io() {
    let (temp, location, store, epoch) = fixture();
    let path = temp.path().join("canonical");
    let Bound {
        registry,
        session,
        mut peer,
        listener,
        responder: _,
    } = bound(&location, &epoch, &path);
    let startup = super::super::startup_exclusion::Startup::hold(&location).unwrap();
    assert!(Context::at(&location, session, &path).is_err());
    assert_ended(&registry, &mut peer, &epoch);
    // Refusal occurs at the actual held boundary, before any fresh socket query.
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(location.handoff_lock().is_err());
    assert!(inspect_db(&store.db, &location.configuration)
        .unwrap()
        .is_none());
    drop(startup);
    assert!(location.handoff_lock().is_ok());
    assert!(super::super::presence_admission::fence(&location).is_ok());
}

#[test]
fn both_retained_phases_block_replacement_after_all_preparer_locks_are_released() {
    let (_temp, location, mut store, epoch) = fixture();
    let presence = super::super::presence_admission::fence(&location).unwrap();
    let lock = location.handoff_lock().unwrap();
    let original = ticket(save(&location, &lock, &presence, &mut store, binding(&epoch)).unwrap());
    drop(lock);
    assert!(super::super::startup_exclusion::Startup::hold(&location).is_err());
    let lock = location.handoff_lock().unwrap();
    request_abort(&location, &lock, &presence, &mut store, &original).unwrap();
    let before = bytes(&store);
    drop(lock);
    drop(presence);
    assert!(super::super::startup_exclusion::Startup::hold(&location).is_err());
    assert_eq!(bytes(&store), before);
    assert_eq!(
        inspect_db(&store.db, &location.configuration)
            .unwrap()
            .unwrap()
            .phase(),
        Phase::AbortRequested
    );
}
