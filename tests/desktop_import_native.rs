//! Real CLI + production actor/socket glue, entirely in private fixture state.
//! The fixture seeds its own durable grant, never an installed service or GUI.
#![cfg(target_os = "linux")]
#[path = "../crates/freedesktop-notify/src/ingress/test_bus.rs"]
mod bus;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::Path,
    process::{Child, Command, Output, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use zerus_desktop_migration::{sha256, SOURCE};

struct Notifications(Arc<AtomicUsize>);
#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        _app: String,
        _replace: u32,
        _icon: String,
        _title: String,
        _body: String,
        _actions: Vec<String>,
        _hints: BTreeMap<String, zbus::zvariant::OwnedValue>,
        _timeout: i32,
    ) -> u32 {
        self.0.fetch_add(1, Ordering::Relaxed);
        1
    }
    fn close_notification(&self, _id: u32) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
struct Fixture {
    root: tempfile::TempDir,
    address: String,
}
impl Fixture {
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hgs"));
        command
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("HOME", self.root.path())
            .env("HGS_CONFIG_DIR", self.root.path().join("config"))
            .env("HGS_STATE_DIR", self.root.path().join("state"))
            .env("HGS_SELF", "example")
            .env("HGS_PEERS", "")
            .env("XDG_RUNTIME_DIR", self.root.path())
            .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
            .env("XDG_SESSION_TYPE", "wayland")
            .env("WAYLAND_DISPLAY", "missing-fixture-display")
            .env("TMPDIR", self.root.path().join("tmp"))
            .env("TMUX_TMPDIR", self.root.path().join("tmux"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
    fn run(&self, args: &[&str], input: &[u8]) -> Output {
        let mut child = Process(self.command(args).spawn().unwrap());
        child.0.stdin.take().unwrap().write_all(input).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        while child.0.try_wait().unwrap().is_none() {
            assert!(Instant::now() < until, "private CLI deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut output = Vec::new();
        let mut error = Vec::new();
        child
            .0
            .stdout
            .take()
            .unwrap()
            .take(128 << 10)
            .read_to_end(&mut output)
            .unwrap();
        child
            .0
            .stderr
            .take()
            .unwrap()
            .take(128 << 10)
            .read_to_end(&mut error)
            .unwrap();
        Output {
            status: child.0.wait().unwrap(),
            stdout: output,
            stderr: error,
        }
    }
    fn json(&self, args: &[&str], input: &[u8]) -> Value {
        let output = self.run(args, input);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn start(&self, background: bool) -> Process {
        let args = if background {
            vec!["desktop", "serve", "--background"]
        } else {
            vec!["desktop", "serve"]
        };
        let child = Process(self.command(&args).spawn().unwrap());
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let status = self.json(&["desktop", "status", "--json"], b"");
            if status["running"] == true {
                return child;
            }
            assert!(Instant::now() < until, "private service readiness");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn stop(&self, child: &mut Process) {
        assert_eq!(unsafe { libc::kill(child.0.id() as i32, libc::SIGTERM) }, 0);
        let until = Instant::now() + Duration::from_secs(8);
        while child.0.try_wait().unwrap().is_none() {
            assert!(Instant::now() < until, "private service drain");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(child.0.wait().unwrap().success());
    }
}
fn item(out: &mut Vec<u8>, header: Value, body: &[u8]) {
    out.extend(serde_json::to_vec(&header).unwrap());
    out.push(b'\n');
    out.extend(body);
    out.push(b'\n');
}
fn bundle(configuration: &str, id: &str) -> (Vec<u8>, String) {
    let mut out=serde_json::to_vec(&json!({"abi":"desktop-import-v1","configuration":configuration,"source":SOURCE,"replay":false,"import_id":id,"max_bytes":1<<20})).unwrap();
    out.push(b'\n');
    let body = br#"{"fixture":"unchanged source"}"#;
    let snapshot = json!({"sha256":sha256(body),"bytes":body.len()});
    let page=serde_json::to_vec(&json!({"version":1,"family":"settings","schema":1,"source_sha256":"b".repeat(64),"index":0,"snapshot":snapshot})).unwrap();
    let reference = json!({"sha256":sha256(&page),"bytes":page.len()});
    let record=serde_json::to_vec(&json!({"version":1,"configuration":configuration,"source":SOURCE,"import_id":id,"evidence":"staged","created_at":1,"automatic_approvals":false,
        "sources":[{"family":"settings","schema":1,"format":"json","sha256":"b".repeat(64),"build":null}],"pages":[reference]})).unwrap();
    item(&mut out, json!({"t":"snapshot","reference":snapshot}), body);
    item(&mut out, json!({"t":"page","reference":reference}), &page);
    item(
        &mut out,
        json!({"t":"record","bytes":record.len()}),
        &record,
    );
    (out, sha256(&record))
}
fn grant(root: &Path) {
    let image = root.join("hgs-tray");
    fs::write(&image, b"private grant fixture").unwrap();
    let meta = fs::metadata(&image).unwrap();
    let origin = json!({"image":image,"device":meta.dev(),"inode":meta.ino(),"modified":meta.mtime(),"modified_nanos":meta.mtime_nsec(),
        "build":{"version":"1.2.3","source_commit":"a".repeat(40),"source_dirty":false}});
    let db = rusqlite::Connection::open(root.join("state/desktop/state.sqlite3")).unwrap();
    db.execute("UPDATE desktop_ownership SET state='service',owner_epoch=?1,origin=?2,rearm_required=0 WHERE id=1",
        rusqlite::params![uuid::Uuid::new_v4().to_string(),serde_json::to_string(&origin).unwrap()]).unwrap();
}
#[test]
fn actual_cli_publishes_through_the_granted_service_actor_and_retains_immutable_identity() {
    let bus = bus::Bus::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let _notifications = zbus::blocking::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name("org.freedesktop.Notifications")
        .unwrap()
        .serve_at(
            "/org/freedesktop/Notifications",
            Notifications(calls.clone()),
        )
        .unwrap()
        .build()
        .unwrap();
    let owner: String = bus
        .connect()
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetNameOwner",
            &("org.freedesktop.Notifications",),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert!(owner.starts_with(':'));
    let fixture = Fixture {
        root: tempfile::tempdir().unwrap(),
        address: bus.address.clone(),
    };
    for name in ["config", "tmp", "tmux"] {
        fs::create_dir(fixture.root.path().join(name)).unwrap();
    }
    let mut passive = fixture.start(false);
    let initial = fixture.json(&["desktop", "status", "--json"], b"");
    let id = uuid::Uuid::new_v4().to_string();
    let (input, hash) = bundle(initial["configuration"].as_str().unwrap(), &id);
    use std::os::unix::fs::OpenOptionsExt;
    let exclusion = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(fixture.root.path().join("state/desktop/handoff.lock"))
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&exclusion).unwrap();
    let negative = fixture.json(&["desktop", "import-evidence", "--json"], &input);
    assert_eq!(
        negative,
        json!({"abi":"desktop-import-v1","published":false,"configuration":initial["configuration"],"import_id":id,
        "import_record":{"version":1,"sha256":hash},"code":"handoff_busy","this_attempt_not_committed":true})
    );
    assert!(!fixture
        .root
        .path()
        .join("state/desktop/imports")
        .join(&id)
        .exists());
    fs2::FileExt::unlock(&exclusion).unwrap();
    drop(exclusion);
    fixture.stop(&mut passive);
    grant(fixture.root.path());
    let mut service = fixture.start(true);
    let until = Instant::now() + Duration::from_secs(10);
    let status = loop {
        let status = fixture.json(&["desktop", "status", "--json"], b"");
        if status["notification_runtime"]["ready"] == true {
            break status;
        }
        assert!(
            Instant::now() < until,
            "native fixture host readiness: {status}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let config = status["configuration"].as_str().unwrap();
    let epoch = status["epoch"].clone();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.root.path().join("state/desktop/handoff.lock"))
        .unwrap();
    assert!(
        fs2::FileExt::try_lock_exclusive(&lock).is_err(),
        "the actual runtime must hold publication exclusion"
    );
    // A real incomplete CLI owns this native stage lease. Age alone cannot
    // authorize its removal; after the original process ends a later commit
    // must perform the bounded sweep through production actor glue.
    let stale_id = uuid::Uuid::new_v4().to_string();
    let mut abandoned = Process(
        fixture
            .command(&["desktop", "import-evidence", "--json"])
            .spawn()
            .unwrap(),
    );
    writeln!(abandoned.0.stdin.as_mut().unwrap(),"{}",json!({"abi":"desktop-import-v1","configuration":config,"source":SOURCE,"replay":false,"import_id":stale_id,"max_bytes":1<<20})).unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    let imports = fixture.root.path().join("state/desktop/imports");
    assert!(imports.is_dir());
    let (stale_marker, stale_path) = loop {
        let found = fs::read_dir(&imports).unwrap().take(32).find_map(|entry| {
            let path = entry.ok()?.path();
            if !path.file_name()?.to_str()?.ends_with(".owner-v1.json") {
                return None;
            }
            let file = fs::File::open(&path).ok()?;
            if file.metadata().ok()?.len() > 4096 {
                return None;
            }
            let mut bytes = vec![];
            file.take(4097).read_to_end(&mut bytes).ok()?;
            if bytes.len() > 4096 {
                return None;
            }
            let mut record: Value = serde_json::from_slice(&bytes).ok()?;
            if record["import_id"] != stale_id {
                return None;
            }
            let stage = imports.join(record["stage"].as_str()?);
            record["created"] = json!(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    - 86460
            );
            fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
            Some((path, stage))
        });
        if let Some(found) = found {
            break found;
        }
        assert!(Instant::now() < until, "original stage creation");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(config, initial["configuration"].as_str().unwrap());
    let report = fixture.json(&["desktop", "import-evidence", "--json"], &input);
    assert_eq!(report["published"], true);
    assert_eq!(report["import_id"], id);
    assert_eq!(report["import_record"]["sha256"], hash);
    let after = fixture.json(&["desktop", "status", "--json"], b"");
    assert_eq!(after["epoch"], epoch);
    assert_eq!(after["notification_runtime"]["ready"], true);
    assert!(
        stale_path.is_dir() && stale_marker.is_file(),
        "live creator retained despite old timestamp"
    );
    abandoned.0.kill().unwrap();
    abandoned.0.wait().unwrap();
    let second = uuid::Uuid::new_v4().to_string();
    let (second_input, _) = bundle(config, &second);
    assert_eq!(
        fixture.json(&["desktop", "import-evidence", "--json"], &second_input)["published"],
        true
    );
    assert!(
        !stale_path.exists() && !stale_marker.exists(),
        "production commit sweeps the original abandoned stage after its lease ends"
    );
    assert!(service.0.try_wait().unwrap().is_none());
    assert!(fs2::FileExt::try_lock_exclusive(&lock).is_err());
    let lookup = fixture.json(
        &["desktop", "import-status", "--json", "--import-id", &id],
        b"",
    );
    assert_eq!(lookup["record_present"], true);
    assert_eq!(lookup["import_record"]["sha256"], hash);
    assert!(
        !fixture
            .run(&["desktop", "import-evidence", "--json"], &input)
            .status
            .success(),
        "same UUID is never overwritten"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "publication never posts/closes a notification"
    );
    fixture.json(&["desktop", "revoke", "--json"], b"");
    fixture.stop(&mut service);
}
