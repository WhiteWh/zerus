//! Real bounded cache workers/files/SQLite/owner exclusion with synthetic native
//! permits. These tests do not claim a real frontend authentication ceremony.
use super::*;
use crate::{config::Config, desktop::revoke};
use std::fs;

struct Fixture {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    location: Location,
    runtime: Runtime,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let config = Config {
            home: root.to_str().unwrap().into(),
            dir: root.join("config"),
            state: root.join("state"),
            selves: vec![],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        };
        fs::create_dir_all(&config.dir).unwrap();
        let location = Location::new(&config).unwrap();
        location.prepare().unwrap();
        let owner = Arc::new(location.lock().unwrap());
        let epoch = id();
        let store = Store::open(&location, &epoch).unwrap();
        let runtime = Runtime::new(&location, &epoch, owner);
        Self {
            _temp: temp,
            root,
            location,
            runtime,
            store,
        }
    }
    fn save(&self) -> std::path::PathBuf {
        let root = self.root.join("cache");
        let part = zerus_client_core::download_cache::reserve(&root).unwrap();
        fs::write(part.path(), b"cached original").unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert!(zerus_client_core::download_cache::complete(
            part,
            "report.txt",
            None,
            now - 120_000
        )
        .unwrap()
        .warning
        .is_none());
        root
    }
    fn prepare(&mut self, root: &Path, revision: u64) -> (String, String) {
        self.prepare_timing(root, revision, "immediate")
    }
    fn prepare_timing(&mut self, root: &Path, revision: u64, timing: &str) -> (String, String) {
        let prepare = id();
        let operation = id();
        let value = json!({"op":"cache_maintenance","request_id":prepare,"command":{"phase":"prepare",
            "operation_id":operation,"configuration":self.location.configuration,"root":root,"limit":1,"revision":revision,"timing":timing,"policy_change":true}});
        let result = self
            .runtime
            .frontend_request(&mut self.store, Validated::fixture(&value));
        assert_eq!(result["phase"], "preparing", "{result}");
        assert_eq!(self.store.operation_undrained().unwrap(), 1);
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            self.runtime.poll(&mut self.store);
            let row = ledger::load(&self.store.db, &prepare).unwrap().unwrap();
            if row.phase == ledger::Phase::Prepared {
                break;
            }
            assert!(
                Instant::now() < end,
                "cache preparation failed: {:?}",
                row.phase
            );
            thread::sleep(Duration::from_millis(2));
        }
        (prepare, operation)
    }
    fn execute(&mut self, prepare: &str, operation: &str) -> Value {
        let row = ledger::load(&self.store.db, prepare).unwrap().unwrap();
        let value = json!({"op":"cache_maintenance","request_id":operation,"command":{"phase":"execute",
            "configuration":self.location.configuration,"preparation_id":prepare,"binding_sha256":row.binding_sha256}});
        self.runtime
            .frontend_request(&mut self.store, Validated::fixture(&value))
    }
    fn drain(&mut self) {
        let end = Instant::now() + Duration::from_secs(5);
        while !self.runtime.drained() {
            self.runtime.poll(&mut self.store);
            assert!(
                Instant::now() < end,
                "cache worker/SQL did not physically drain"
            );
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(self.store.operation_undrained().unwrap(), 0);
    }
}

#[test]
fn prepared_data_does_not_delete_and_exact_admission_outlives_the_native_caller() {
    let mut f = Fixture::new();
    let root = f.save();
    let (prepare, operation) = f.prepare(&root, 0);
    assert_eq!(
        fs::read(root.join("report.txt")).unwrap(),
        b"cached original"
    );
    let row = ledger::load(&f.store.db, &prepare).unwrap().unwrap();
    let actual = f.runtime.job.as_ref().unwrap();
    assert_eq!(actual.binding_sha256, row.binding_sha256);
    let result = f.execute(&prepare, &operation);
    assert_eq!(result["accepted"], true);
    assert_eq!(result["phase"], "reserved");
    // The native fixture permit is consumed/dropped. The original service worker
    // retains its real resources and finishes without another client request.
    f.drain();
    assert!(!root.join("report.txt").exists());
    let result = query(
        &f.store,
        &f.location.configuration,
        &json!({"op":"cache_maintenance_result",
        "request_id":id(),"original_request_id":operation}),
    );
    assert_eq!(result["found"], true);
    assert_eq!(result["result"]["phase"], "settled");
    assert_eq!(result["result"]["removed"], 1);
    assert_eq!(result["result"]["drained"], true);
    assert_eq!(f.execute(&prepare, &operation)["phase"], "settled");
}

#[test]
fn real_preparation_and_reserved_resources_block_global_settlement_until_joined() {
    let mut f = Fixture::new();
    let root = f.save();
    let (_prepare, operation) = f.prepare(&root, 0);
    assert_eq!(f.store.operation_status()["undrained"], 1);
    let revoke_id = id();
    let pending = revoke::request(
        &f.location,
        &mut f.store,
        &json!({"op":"revoke","request_id":revoke_id,"reason":"manual"}),
    );
    assert_eq!(pending["code"], "operation_resources_not_drained");
    assert_eq!(pending["checkpointed"], false);
    f.runtime.request_stop(&mut f.store).unwrap();
    assert!(!f.runtime.drained()); // a cancellation flag is not a physical join
    assert_eq!(f.store.operation_status()["cache_undrained"], 1);
    f.drain();
    let settled = revoke::request(
        &f.location,
        &mut f.store,
        &json!({"op":"revoke","request_id":revoke_id,"reason":"manual"}),
    );
    assert_eq!(settled["checkpointed"], true, "{settled}");
    assert!(root.join("report.txt").is_file());
    let row = ledger::load(&f.store.db, &operation).unwrap().unwrap();
    assert_eq!(row.phase, ledger::Phase::Refused);
}

#[test]
fn a_changed_index_cannot_inherit_its_frozen_delete_request() {
    let mut f = Fixture::new();
    let root = f.save();
    let (prepare, operation) = f.prepare(&root, 0);
    fs::write(
        root.join(".zerus-files.json"),
        b"{\"version\":2,\"files\":[]}",
    )
    .unwrap();
    assert_eq!(f.execute(&prepare, &operation)["accepted"], true);
    f.drain();
    let result = ledger::load(&f.store.db, &operation).unwrap().unwrap();
    assert_eq!(result.phase, ledger::Phase::Settled);
    assert_eq!(result.receipt.unwrap().files, [ledger::FileState::Kept]);
    assert_eq!(
        fs::read(root.join("report.txt")).unwrap(),
        b"cached original"
    );
}

#[test]
fn closed_raw_requests_and_foreign_configuration_never_prepare_or_execute() {
    let mut f = Fixture::new();
    let root = f.save();
    let raw = query(
        &f.store,
        &f.location.configuration,
        &json!({"op":"cache_maintenance",
        "request_id":id(),"confirmed":true}),
    );
    assert_eq!(raw["accepted"], false);
    let value = json!({"op":"cache_maintenance","request_id":id(),"command":{"phase":"prepare",
        "operation_id":id(),"configuration":"f".repeat(64),"root":root,"limit":1,"revision":0,"timing":"immediate","policy_change":true}});
    assert_eq!(
        f.runtime
            .frontend_request(&mut f.store, Validated::fixture(&value))["accepted"],
        false
    );
    assert!(f.runtime.drained());
    assert!(root.join("report.txt").is_file());
}

#[test]
fn schema_upgrades_exactly_from_real_creation_seventeen_without_a_placeholder() {
    let mut f = Fixture::new();
    // Reconstruct the exact prior version by removing only this leaf's DDL;
    // the real creation17 and notification-controls16 tables remain unchanged.
    f.store.db.execute_batch("DROP TRIGGER desktop_cache_unknown_preserve; DROP TRIGGER desktop_cache_unknown_immutable;
        DROP TABLE desktop_cache_unknown; DROP TABLE desktop_cache_files; DROP TABLE desktop_cache_operations;
        DROP TABLE desktop_cache_roots; PRAGMA user_version=17;").unwrap();
    let creation_before: Vec<(String, String)> = f
        .store
        .db
        .prepare(
            "SELECT name,sql FROM sqlite_schema WHERE name LIKE 'desktop_creation_%' ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(!creation_before.is_empty());
    let tx = f.store.db.transaction().unwrap();
    super::migrate(&tx).unwrap();
    tx.pragma_update(None, "user_version", SCHEMA).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        f.store
            .db
            .query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        18
    );
    assert_eq!(f.store.db.query_row::<i64,_,_>("SELECT count(*) FROM pragma_table_info('desktop_cache_operations') WHERE name='effect_started'",[],|r|r.get(0)).unwrap(),0);
    drop(f.store);
    let epoch = id();
    let store = Store::open(&f.location, &epoch).unwrap();
    assert_eq!(
        store
            .db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        super::super::store::SCHEMA
    );
    assert_eq!(SCHEMA, 18);
    assert_eq!(super::super::creation_clone::SCHEMA, 17);
    let creation_after: Vec<(String, String)> = store
        .db
        .prepare(
            "SELECT name,sql FROM sqlite_schema WHERE name LIKE 'desktop_creation_%' ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(creation_before, creation_after);
    assert_eq!(store.operation_undrained().unwrap(), 0);
}

#[test]
fn effect_schema_upgrades_exactly_from_eighteen_and_rolls_back_partial_ddl() {
    let f = Fixture::new();
    assert_eq!(super::super::cache_effects::SCHEMA, 19);
    f.store.db.execute_batch("DROP TRIGGER desktop_cache_effect_monotone; DROP TRIGGER desktop_cache_kept_original;
        ALTER TABLE desktop_cache_operations DROP COLUMN effect_started;
        ALTER TABLE desktop_cache_operations DROP COLUMN kept_receipt; PRAGMA user_version=18;
        CREATE TRIGGER desktop_cache_effect_monotone BEFORE UPDATE ON desktop_cache_operations BEGIN SELECT 1; END;").unwrap();
    assert!(Store::open(&f.location, &id()).is_err());
    assert_eq!(
        f.store
            .db
            .query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        18
    );
    assert_eq!(f.store.db.query_row::<i64,_,_>("SELECT count(*) FROM pragma_table_info('desktop_cache_operations') WHERE name IN('effect_started','kept_receipt')",[],|r|r.get(0)).unwrap(),0);
    f.store
        .db
        .execute_batch("DROP TRIGGER desktop_cache_effect_monotone;")
        .unwrap();
    drop(f.store);
    let store = Store::open(&f.location, &id()).unwrap();
    assert_eq!(
        store
            .db
            .query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        19
    );
    assert_eq!(store.operation_undrained().unwrap(), 0);
}

#[test]
fn deadline_keeps_the_actual_worker_owned_and_late_completion_cannot_erase_unknown() {
    let mut f = Fixture::new();
    let root = f.save();
    let (entered, wait) = mpsc::sync_channel(1);
    let (release, receive) = mpsc::sync_channel(1);
    f.runtime.hold = Some(Arc::new(TestHold {
        entered,
        release: std::sync::Mutex::new(receive),
        after_started: true,
    }));
    let (prepare, operation) = f.prepare(&root, 0);
    assert_eq!(f.execute(&prepare, &operation)["accepted"], true);
    let until = Instant::now() + Duration::from_secs(3);
    while wait.try_recv().is_err() {
        f.runtime.poll(&mut f.store);
        assert!(Instant::now() < until, "original hold did not become ready");
        thread::sleep(Duration::from_millis(1));
    }
    f.runtime.job.as_mut().unwrap().until = Instant::now() - Duration::from_millis(1);
    f.runtime.poll(&mut f.store);
    let row = ledger::load(&f.store.db, &operation).unwrap().unwrap();
    assert_eq!(row.phase, ledger::Phase::Unknown);
    assert!(row.undrained);
    assert!(!f.runtime.drained());
    assert_eq!(f.store.operation_undrained().unwrap(), 1);
    assert!(
        zerus_client_core::qt_lock::lock(&root.join(".zerus-files.lock"), Duration::ZERO)
            .unwrap()
            .is_none()
    );
    release.send(()).unwrap();
    f.drain();
    let row = ledger::load(&f.store.db, &operation).unwrap().unwrap();
    assert_eq!(row.phase, ledger::Phase::Unknown);
    assert!(row.receipt.is_none());
    assert!(root.join("report.txt").is_file());
    // Physical join/owner release precedes a genuine store/runtime reopen.
    drop(f.runtime);
    drop(f.store);
    let owner = Arc::new(f.location.lock().unwrap());
    let epoch = id();
    f.store = Store::open(&f.location, &epoch).unwrap();
    f.runtime = Runtime::new(&f.location, &epoch, owner);
    let (next_prepare, next_operation) = f.prepare(&root, 1);
    assert_eq!(f.execute(&next_prepare, &next_operation)["accepted"], true);
    f.drain();
    let kept = ledger::load(&f.store.db, &next_operation).unwrap().unwrap();
    assert_eq!(kept.receipt.unwrap().files, [ledger::FileState::Kept]);
    assert!(root.join("report.txt").is_file());
    assert_eq!(
        ledger::load(&f.store.db, &operation)
            .unwrap()
            .unwrap()
            .phase,
        ledger::Phase::Unknown
    );
}

/// Private child entry for the real process-loss control below. No production
/// CLI flag, current native permit or owner configuration is supplied here.
#[test]
fn owned_cache_crash_fixture() {
    let Some(base) = std::env::var_os("ZERUS_CACHE_CRASH_FIXTURE") else {
        return;
    };
    let base = std::path::PathBuf::from(base);
    let mut f = Fixture::new();
    let root = f.save();
    let (entered, wait) = mpsc::sync_channel(1);
    let (release, receive) = mpsc::sync_channel(1);
    let after_started = std::env::var("ZERUS_CACHE_CRASH_AFTER_START").as_deref() == Ok("1");
    f.runtime.hold = after_started.then(|| {
        Arc::new(TestHold {
            entered,
            release: std::sync::Mutex::new(receive),
            after_started: true,
        })
    });
    let (prepare, operation) = f.prepare_timing(
        &root,
        0,
        if after_started {
            "immediate"
        } else {
            "after_opening_grace"
        },
    );
    assert_eq!(f.execute(&prepare, &operation)["accepted"], true);
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        f.runtime.poll(&mut f.store);
        let ready = if after_started {
            wait.try_recv().is_ok()
        } else {
            zerus_client_core::qt_lock::lock(&root.join(".zerus-files.lock"), Duration::ZERO)
                .unwrap()
                .is_some()
        };
        if ready {
            break;
        }
        assert!(
            Instant::now() < until,
            "original crash stage did not become ready"
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        ledger::load(&f.store.db, &operation)
            .unwrap()
            .unwrap()
            .effect_started,
        after_started
    );
    let marker = json!({"root":f.root,"state":f.location.root,"configuration":f.location.configuration,"operation_id":operation});
    // Only the parent's unpredictable private marker folder is used. Its
    // original child identity, not the marker, authorizes parent termination.
    let pending = base.join("ready.pending");
    fs::write(&pending, serde_json::to_vec(&marker).unwrap()).unwrap();
    fs::rename(pending, base.join("ready.json")).unwrap();
    let end = Instant::now() + Duration::from_secs(30);
    while Instant::now() < end {
        thread::sleep(Duration::from_millis(20));
    }
    // Controller failure has a bounded child safety path; never execute the
    // paused unlink after its original controller has disappeared.
    f.runtime.request_stop(&mut f.store).unwrap();
    let _ = release.send(());
    f.drain();
}

#[test]
fn killing_during_the_real_grace_recovers_original_kept_without_an_unknown_fence() {
    kill_original_and_recover(false);
}
#[test]
fn killing_after_committed_effect_start_recovers_unknown_and_never_replays() {
    kill_original_and_recover(true);
}
fn kill_original_and_recover(after_started: bool) {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let base = tempfile::tempdir().unwrap().keep();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "desktop::cache_maintenance::tests::owned_cache_crash_fixture",
            "--nocapture",
        ])
        .env("ZERUS_CACHE_CRASH_FIXTURE", &base)
        .env(
            "ZERUS_CACHE_CRASH_AFTER_START",
            if after_started { "1" } else { "0" },
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    let marker = base.join("ready.json");
    while !marker.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "original fixture exited before reservation; retained private files"
        );
        assert!(
            Instant::now() < end,
            "original fixture did not reserve; retained private files"
        );
        thread::sleep(Duration::from_millis(2));
    }
    let mut bytes = Vec::new();
    fs::File::open(&marker)
        .unwrap()
        .take(16 * 1024 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 16 * 1024);
    let marker: Value = serde_json::from_slice(&bytes).unwrap();
    let original_birth = platform::process_identity(child.id()).unwrap();
    child.kill().unwrap(); // ONLY this still-owned original child, never a group
    let end = Instant::now() + Duration::from_secs(3);
    while child.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < end,
            "original termination remains pending; retained private files"
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert_ne!(
        platform::process_identity(child.id()).as_ref(),
        Some(&original_birth)
    );
    let root = std::path::PathBuf::from(marker["root"].as_str().unwrap());
    let location = Location {
        root: std::path::PathBuf::from(marker["state"].as_str().unwrap()),
        configuration: marker["configuration"].as_str().unwrap().into(),
    };
    let owner = location.lock().unwrap();
    let store = Store::open(&location, &id()).unwrap();
    let original = ledger::load(&store.db, marker["operation_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(original.effect_started, after_started);
    assert_eq!(
        original.phase,
        if after_started {
            ledger::Phase::Unknown
        } else {
            ledger::Phase::Settled
        }
    );
    assert!(!original.undrained);
    if !after_started {
        assert_eq!(
            original.receipt.as_ref().unwrap().files,
            [ledger::FileState::Kept]
        );
        assert_eq!(original.receipt, original.kept_receipt);
    }
    let content = &original.binding.unwrap().candidates[0].content_id;
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM desktop_cache_unknown WHERE content_id=?1)",
                [content],
                |r| r.get::<_, bool>(0)
            )
            .unwrap(),
        after_started
    );
    assert!(root.join("cache/report.txt").is_file());
    drop(store);
    drop(owner);
    // The fixture's entire private root is known from this original child;
    // removal follows its actual successful physical wait and owner exclusion.
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn preference_can_be_saved_before_the_first_download_without_effect_permission() {
    let mut f = Fixture::new();
    let root = f.root.join("missing-cache");
    let prepare = id();
    let operation = id();
    let value = json!({"op":"cache_maintenance","request_id":prepare,"command":{"phase":"prepare",
        "operation_id":operation,"configuration":f.location.configuration,"root":root,"limit":128,"revision":0,
        "timing":"immediate","policy_change":true}});
    let receipt = f
        .runtime
        .frontend_request(&mut f.store, Validated::fixture(&value));
    assert_eq!(receipt["policy_saved"], true);
    assert_eq!(receipt["accepted"], false);
    assert_eq!(receipt["revision"], 1);
    f.drain();
    assert!(!root.exists());
    let policy = query(
        &f.store,
        &f.location.configuration,
        &json!({"op":"cache_maintenance_policy","request_id":id(),"root":root}),
    );
    assert_eq!(policy["configured"], true);
    assert_eq!(policy["limit"], 128);
    assert_eq!(
        ledger::load(&f.store.db, &operation)
            .unwrap()
            .unwrap()
            .phase,
        ledger::Phase::Refused
    );
}

#[test]
fn delayed_original_releases_qt_lock_and_revoke_joins_without_waiting_for_the_grace() {
    let mut f = Fixture::new();
    let root = f.save();
    let (prepare, operation) = f.prepare_timing(&root, 0, "after_opening_grace");
    assert_eq!(f.execute(&prepare, &operation)["accepted"], true);
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(guard) =
            zerus_client_core::qt_lock::lock(&root.join(".zerus-files.lock"), Duration::ZERO)
                .unwrap()
        {
            drop(guard);
            break;
        }
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(f.store.operation_undrained().unwrap(), 1);
    f.runtime.request_stop(&mut f.store).unwrap();
    f.drain();
    assert!(root.join("report.txt").is_file());
}

#[test]
fn admitted_delayed_original_completes_after_the_full_real_opening_grace() {
    let mut f = Fixture::new();
    let root = f.save();
    let (prepare, operation) = f.prepare_timing(&root, 0, "after_opening_grace");
    let admitted = Instant::now();
    assert_eq!(f.execute(&prepare, &operation)["accepted"], true);
    let until = admitted + Duration::from_secs(75);
    while !f.runtime.drained() {
        f.runtime.poll(&mut f.store);
        if admitted.elapsed() < Duration::from_secs(60) {
            assert!(root.join("report.txt").is_file());
        }
        assert!(
            Instant::now() < until,
            "original delayed pass did not drain"
        );
        thread::sleep(Duration::from_millis(25));
    }
    assert!(admitted.elapsed() >= Duration::from_secs(60));
    let row = ledger::load(&f.store.db, &operation).unwrap().unwrap();
    assert_eq!(row.phase, ledger::Phase::Settled);
    assert_eq!(row.receipt.unwrap().files, [ledger::FileState::Removed]);
    assert!(!root.join("report.txt").exists());
}

#[test]
fn a_missing_original_start_ack_never_unlinks_or_invents_an_unknown_fence() {
    let mut f = Fixture::new();
    let root = f.save();
    let (prepare, operation) = f.prepare(&root, 0);
    f.execute(&prepare, &operation);
    let event = f
        .runtime
        .job
        .as_ref()
        .unwrap()
        .events
        .recv_timeout(Duration::from_secs(3))
        .unwrap();
    let Event::StartEffects { reply, .. } = event else {
        panic!("original worker did not ask for start")
    };
    assert!(
        !ledger::load(&f.store.db, &operation)
            .unwrap()
            .unwrap()
            .effect_started
    );
    assert_eq!(f.store.operation_undrained().unwrap(), 1);
    assert!(root.join("report.txt").is_file());
    drop(reply); // Only the original private reply; no actor commit or permit.
    f.drain();
    let row = ledger::load(&f.store.db, &operation).unwrap().unwrap();
    assert!(!row.effect_started);
    assert_eq!(row.phase, ledger::Phase::Settled);
    assert_eq!(
        row.receipt.as_ref().unwrap().files,
        [ledger::FileState::Kept]
    );
    assert_eq!(row.receipt, row.kept_receipt);
    assert!(root.join("report.txt").is_file());
    assert_eq!(
        f.store
            .db
            .query_row::<i64, _, _>("SELECT count(*) FROM desktop_cache_unknown", [], |r| r
                .get(0))
            .unwrap(),
        0
    );
}

#[test]
fn a_start_commit_write_failure_does_not_release_a_permit_or_unlink() {
    let mut f = Fixture::new();
    let root = f.save();
    let (prepare, operation) = f.prepare(&root, 0);
    f.execute(&prepare, &operation);
    f.store.db.execute_batch("PRAGMA query_only=ON;").unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    while f
        .runtime
        .job
        .as_ref()
        .is_some_and(|j| !j.cancel.load(Ordering::Acquire))
        && !f.runtime.error
    {
        f.runtime.poll(&mut f.store);
        assert!(Instant::now() < until, "start failure was not observed");
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        !ledger::load(&f.store.db, &operation)
            .unwrap()
            .unwrap()
            .effect_started
    );
    assert!(root.join("report.txt").is_file());
    f.store.db.execute_batch("PRAGMA query_only=OFF;").unwrap();
    f.drain();
    let row = ledger::load(&f.store.db, &operation).unwrap().unwrap();
    assert_eq!(row.phase, ledger::Phase::Settled);
    assert!(!row.effect_started);
    assert_eq!(
        row.receipt.as_ref().unwrap().files,
        [ledger::FileState::Kept]
    );
    assert!(root.join("report.txt").is_file());
}

#[test]
fn a_lost_reply_after_the_start_commit_still_does_not_authorize_unlink() {
    let mut f = Fixture::new();
    let root = f.save();
    let (prepare, operation) = f.prepare(&root, 0);
    f.execute(&prepare, &operation);
    let job = f.runtime.job.as_ref().unwrap();
    let event = job.events.recv_timeout(Duration::from_secs(3)).unwrap();
    let Event::StartEffects {
        binding_sha256,
        reply,
    } = event
    else {
        panic!("original worker did not ask for start")
    };
    assert_eq!(job.binding_sha256.as_ref(), Some(&binding_sha256));
    let original = job.original.clone();
    // Transport-loss control: the original storage transition commits, but no
    // EffectPermit is delivered. This is not a native authentication proof.
    let tx = f
        .store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    ledger::start_effects(&tx, &original, &binding_sha256).unwrap();
    tx.commit().unwrap();
    assert!(
        ledger::load(&f.store.db, &operation)
            .unwrap()
            .unwrap()
            .effect_started
    );
    drop(reply);
    f.drain();
    let row = ledger::load(&f.store.db, &operation).unwrap().unwrap();
    assert_eq!(row.phase, ledger::Phase::Settled);
    assert!(row.effect_started);
    assert_eq!(
        row.receipt.as_ref().unwrap().files,
        [ledger::FileState::Kept]
    );
    assert!(root.join("report.txt").is_file());
}
