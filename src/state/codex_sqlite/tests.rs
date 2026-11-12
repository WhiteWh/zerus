use super::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    rc::Rc,
    time::{Duration, Instant},
};

const CHILD_ID: &str = "22222222-2222-4222-8222-222222222222";
const OTHER_ID: &str = "33333333-3333-4333-8333-333333333333";

fn until() -> Instant {
    Instant::now() + Duration::from_secs(2)
}

fn stamp(path: &Path) -> Value {
    let Ok((identity, metadata)) = crate::platform::FileIdentity::at(path) else {
        return Value::Null;
    };
    let bytes = fs::read(path).unwrap();
    json!({
        "device": identity.device,
        "inode": identity.inode,
        "bytes": metadata.len(),
        "modified": identity.modified,
        "modified_nanos": identity.modified_nanos,
        "sha256": format!("{:x}", Sha256::digest(&bytes)),
    })
}

fn stamps(home: &Path) -> Value {
    json!({
        "main": stamp(&home.join("state_5.sqlite")),
        "wal": stamp(&home.join("state_5.sqlite-wal")),
        "shm": stamp(&home.join("state_5.sqlite-shm")),
    })
}

struct Writer {
    _directory: tempfile::TempDir,
    child: Child,
    home: PathBuf,
    ready: Value,
    stopped: bool,
}
impl Writer {
    fn finish(&mut self) {
        if self.stopped {
            return;
        }
        fs::write(self.home.join("stop"), "").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "writer failed");
                self.stopped = true;
                return;
            }
            assert!(Instant::now() < deadline, "writer did not settle");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        let _ = fs::write(self.home.join("stop"), "");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if self.child.try_wait().ok().flatten().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn start(case: &str) -> Writer {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().to_path_buf();
    let stdout = fs::File::create(home.join("stdout")).unwrap();
    let stderr = fs::File::create(home.join("stderr")).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "state::codex_sqlite::tests::writer_process",
            "--ignored",
            "--test-threads=1",
        ])
        .env("ZERUS_CODEX_WRITER", case)
        .env("CODEX_WRITER_DIR", &home)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let ready = loop {
        let path = home.join("ready.json");
        if path.exists() {
            break serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "writer exited before ready: {}",
            fs::read_to_string(home.join("stderr")).unwrap_or_default()
        );
        assert!(Instant::now() < deadline, "writer was not ready");
        std::thread::sleep(Duration::from_millis(10));
    };
    Writer {
        _directory: directory,
        child,
        home,
        ready,
        stopped: false,
    }
}

#[test]
#[ignore = "private writer process"]
fn writer_process() {
    let Ok(case) = std::env::var("ZERUS_CODEX_WRITER") else {
        return;
    };
    let home = PathBuf::from(std::env::var_os("CODEX_WRITER_DIR").unwrap());
    let database = home.join("state_5.sqlite");
    let rollout = home.join("rollout-child.jsonl");
    let db = Connection::open(&database).unwrap();
    db.execute_batch(
        "PRAGMA journal_mode=WAL;
        CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT);
        PRAGMA wal_checkpoint(TRUNCATE);",
    )
    .unwrap();
    let main_before = fs::read(&database).unwrap();
    let main_contains_child = String::from_utf8_lossy(&main_before).contains(CHILD_ID);
    db.execute(
        "INSERT INTO threads VALUES(?1,?2)",
        rusqlite::params![CHILD_ID, rollout.to_str().unwrap()],
    )
    .unwrap();
    // A WAL restarted after a complete checkpoint: SQLite's next commit writes
    // a new header salt and overwrites from the start, leaving earlier frames
    // with the old salt after its own, which every SQLite reader ignores.
    if case == "restarted" {
        db.execute(
            "INSERT OR REPLACE INTO threads VALUES(?1,?2)",
            rusqlite::params!["other-before-restart", rollout.to_str().unwrap()],
        )
        .unwrap();
        db.execute_batch("PRAGMA wal_checkpoint(PASSIVE)").unwrap();
        db.execute(
            "INSERT OR REPLACE INTO threads VALUES(?1,?2)",
            rusqlite::params!["other-after-restart", rollout.to_str().unwrap()],
        )
        .unwrap();
    }
    // A transaction still being written: a small page cache spills its pages
    // into the WAL as frames without a commit (the child's new path included).
    if case == "uncommitted" {
        db.execute_batch("PRAGMA cache_size=1; BEGIN;").unwrap();
        db.execute(
            "UPDATE threads SET rollout_path=?2 WHERE id=?1",
            rusqlite::params![
                CHILD_ID,
                home.join("rollout-uncommitted.jsonl").to_str().unwrap()
            ],
        )
        .unwrap();
        for index in 0..64 {
            db.execute(
                "INSERT INTO threads VALUES(?1,?2)",
                rusqlite::params![format!("spill-{index}"), "x".repeat(6000)],
            )
            .unwrap();
        }
    }
    let ready = json!({
        "main_contains_child": main_contains_child,
        "stamps": stamps(&home),
    });
    // Published whole: the test may read it the moment it appears.
    fs::write(
        home.join("ready.tmp"),
        serde_json::to_string(&ready).unwrap(),
    )
    .unwrap();
    fs::rename(home.join("ready.tmp"), home.join("ready.json")).unwrap();
    // On request, the main file changes in place (same inode, new modification
    // time), as a live Codex writes: one commit and a PASSIVE checkpoint, or a
    // full rewrite (the child's new rollout path, VACUUM, TRUNCATE checkpoint).
    if case == "checkpoint" || case == "rewrite" {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !home.join("go").exists() && !home.join("stop").exists() && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        if home.join("go").exists() {
            if case == "checkpoint" {
                db.execute(
                    "INSERT OR REPLACE INTO threads VALUES(?1,?2)",
                    rusqlite::params!["other-checkpoint", rollout.to_str().unwrap()],
                )
                .unwrap();
                db.execute_batch("PRAGMA wal_checkpoint(PASSIVE)").unwrap();
            } else {
                db.execute(
                    "UPDATE threads SET rollout_path=?2 WHERE id=?1",
                    rusqlite::params![
                        CHILD_ID,
                        home.join("rollout-rewritten.jsonl").to_str().unwrap()
                    ],
                )
                .unwrap();
                db.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")
                    .unwrap();
            }
            fs::write(home.join("done"), "").unwrap();
        }
    }
    if case == "busy" {
        let mut actions = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(home.join("actions.jsonl"))
            .unwrap();
        for index in 0..200 {
            if home.join("stop").exists() {
                break;
            }
            db.execute(
                "INSERT OR REPLACE INTO threads VALUES(?1,?2)",
                rusqlite::params![format!("other-{index}"), rollout.to_str().unwrap()],
            )
            .unwrap();
            if index % 3 == 0 {
                db.execute_batch("PRAGMA wal_checkpoint(PASSIVE)").unwrap();
            }
            writeln!(
                actions,
                "{}",
                json!({"action": index, "stamps": stamps(&home)})
            )
            .unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while !home.join("stop").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(db);
}

fn isolated(fixture: &str) {
    let state = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("state::codex_sqlite::tests::{fixture}"),
            "--ignored",
            "--test-threads=1",
        ])
        .env("HGS_STATE_DIR", state.path())
        .env("HGS_CONFIG_DIR", state.path().join("config"))
        .env("ZERUS_CODEX_SQLITE_FIXTURE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn directories() -> Rc<RefCell<Vec<PathBuf>>> {
    let directories = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&directories);
    AFTER_DIRECTORY.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |path| {
            captured.borrow_mut().push(path.to_path_buf());
        }));
    });
    directories
}

fn assert_removed(directories: &Rc<RefCell<Vec<PathBuf>>>) {
    assert!(!directories.borrow().is_empty());
    for path in directories.borrow().iter() {
        assert!(!path.exists(), "snapshot retained {path:?}");
    }
    AFTER_DIRECTORY.with(|hook| {
        hook.borrow_mut().take();
    });
}

#[test]
fn a_native_connection_refuses_before_any_raw_open() {
    isolated("native_connection_exclusion");
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn native_connection_exclusion() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().to_path_buf();
    let database = home.join("state_5.sqlite");
    let db = Connection::open(&database).unwrap();
    db.execute_batch("CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT)")
        .unwrap();
    drop(db);
    let native = NativeConnection::open(database).unwrap();
    RAW_OPENS.store(0, std::sync::atomic::Ordering::Relaxed);
    let error = rollout_paths(&home, &[CHILD_ID.into()], until()).unwrap_err();
    assert!(error.contains("already open"), "{error}");
    assert_eq!(RAW_OPENS.load(std::sync::atomic::Ordering::Relaxed), 0);
    drop(native);
}

#[test]
fn a_current_wal_child_is_read_and_the_copy_is_removed() {
    isolated("current_wal_child");
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn current_wal_child() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let mut writer = start("positive");
    let directories = directories();
    let paths = rollout_paths(&writer.home, &[CHILD_ID.into()], until()).unwrap();
    assert_eq!(
        paths[CHILD_ID],
        writer.home.join("rollout-child.jsonl").to_string_lossy()
    );
    assert_eq!(writer.ready["main_contains_child"], false);
    assert_eq!(stamps(&writer.home), writer.ready["stamps"]);
    assert_removed(&directories);
    writer.finish();
}

#[test]
fn a_busy_writer_can_be_read_without_native_side_effects() {
    isolated("busy_writer");
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn busy_writer() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let mut writer = start("busy");
    let directories = directories();
    let paths = rollout_paths(&writer.home, &[CHILD_ID.into()], until()).unwrap();
    assert_eq!(
        paths[CHILD_ID],
        writer.home.join("rollout-child.jsonl").to_string_lossy()
    );
    let after = stamps(&writer.home);
    // Stopped before its log is read, so no line is half written; the action
    // that produced `after` logged its stamps after its write.
    writer.finish();
    let actions = fs::read_to_string(writer.home.join("actions.jsonl")).unwrap();
    let actions: Vec<Value> = actions
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(!actions.is_empty());
    for name in ["main", "wal", "shm"] {
        assert!(
            after[name] == writer.ready["stamps"][name]
                || actions
                    .iter()
                    .any(|action| action["stamps"][name] == after[name]),
            "unattributed {name} delta: {}",
            after[name]
        );
    }
    println!("before={}", writer.ready["stamps"]);
    println!("after={after}");
    println!("writer_actions={}", actions.len());
    assert_removed(&directories);
    writer.finish();
}

/// zerus-wa90v: runs `between` once, inside the first snapshot attempt, then
/// returns the read's result and the main file's stamps before and after.
fn read_with_one_change(
    case: &str,
    between: impl FnOnce(&Path) + 'static,
) -> (
    Result<std::collections::BTreeMap<String, String>>,
    Value,
    Value,
    Writer,
) {
    let writer = start(case);
    let home = writer.home.clone();
    let before = stamps(&home);
    let directories = directories();
    let mut between = Some(between);
    AFTER_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            if let Some(change) = between.take() {
                // Past the file system's timestamp granularity, so an in-place
                // change gives the same file a new modification time.
                std::thread::sleep(Duration::from_millis(20));
                change(&home);
            }
        }));
    });
    let result = rollout_paths(&writer.home, &[CHILD_ID.into()], until());
    AFTER_READ.with(|hook| {
        hook.borrow_mut().take();
    });
    assert_removed(&directories);
    let after = stamps(&writer.home);
    (
        result,
        before["main"].clone(),
        after["main"].clone(),
        writer,
    )
}

/// Asks the fixture writer for its one in-place change and waits for it.
fn writer_changes(home: &Path) {
    fs::write(home.join("go"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !home.join("done").exists() {
        assert!(
            Instant::now() < deadline,
            "the writer did not change the catalog"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The same file, changed in place: its device and inode stay, its content
/// and modification time change.
fn assert_in_place(before: &Value, after: &Value) {
    assert_eq!(after["device"], before["device"]);
    assert_eq!(after["inode"], before["inode"]);
    assert_ne!(after["sha256"], before["sha256"]);
    assert_ne!(
        (&after["modified"], &after["modified_nanos"]),
        (&before["modified"], &before["modified_nanos"])
    );
}

/// A busy writer that commits and checkpoints into the same main file while a
/// snapshot attempt runs: the reader retries and reads the catalog. Only a
/// different file at the path is a replacement.
#[test]
fn a_checkpoint_between_attempts_is_retried_not_refused_as_a_replacement() {
    isolated("checkpoint_between_attempts");
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn checkpoint_between_attempts() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let (paths, before, after, mut writer) = read_with_one_change("checkpoint", writer_changes);
    assert_in_place(&before, &after);
    println!("main before={before} after={after}");
    assert_eq!(
        paths.unwrap()[CHILD_ID],
        writer.home.join("rollout-child.jsonl").to_string_lossy()
    );
    writer.finish();
}

/// A full rewrite of the same file (VACUUM and a TRUNCATE checkpoint) between
/// attempts is retried and read whole: the new rollout path, never a mix.
#[test]
fn an_in_place_rewrite_between_attempts_is_retried_and_read_consistently() {
    isolated("rewrite_between_attempts");
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn rewrite_between_attempts() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let (paths, before, after, mut writer) = read_with_one_change("rewrite", writer_changes);
    assert_in_place(&before, &after);
    assert_eq!(
        paths.unwrap()[CHILD_ID],
        writer
            .home
            .join("rollout-rewritten.jsonl")
            .to_string_lossy()
    );
    writer.finish();
}

/// Another file renamed into place between attempts is a replacement and is
/// refused, even with identical content, never read as the catalog.
#[test]
fn a_file_replaced_between_attempts_is_refused() {
    isolated("replacement_between_attempts");
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn replacement_between_attempts() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let (result, before, after, mut writer) = read_with_one_change("positive", |home| {
        let database = home.join("state_5.sqlite");
        let copy = home.join("copy.sqlite");
        fs::copy(&database, &copy).unwrap();
        fs::rename(&copy, &database).unwrap();
    });
    assert_ne!(after["inode"], before["inode"]);
    assert_eq!(
        result.unwrap_err(),
        "Codex catalog was replaced during its read"
    );
    writer.finish();
}

/// zerus-wa90v: a live WAL restarted after a complete checkpoint keeps the
/// earlier generation's frames after the current ones (another header salt).
/// SQLite ignores them; the snapshot reads the catalog instead of refusing it.
#[test]
fn a_restarted_wal_with_an_earlier_tail_is_read() {
    isolated("restarted_wal");
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn restarted_wal() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let mut writer = start("restarted");
    let wal = fs::read(writer.home.join("state_5.sqlite-wal")).unwrap();
    let page = u32::from_be_bytes(wal[8..12].try_into().unwrap()) as usize;
    let salts: Vec<bool> = wal[32..]
        .chunks_exact(24 + page)
        .map(|frame| frame[8..16] == wal[16..24])
        .collect();
    // The fixture's WAL holds current frames followed by an earlier tail.
    assert!(
        salts.first() == Some(&true) && salts.last() == Some(&false),
        "{salts:?}"
    );
    let directories = directories();
    let paths = rollout_paths(&writer.home, &[CHILD_ID.into()], until()).unwrap();
    assert_eq!(
        paths[CHILD_ID],
        writer.home.join("rollout-child.jsonl").to_string_lossy()
    );
    assert_removed(&directories);
    writer.finish();
}

/// zerus-wa90v: frames after the WAL's last commit frame belong to a
/// transaction still being written. They are not catalog content, and a torn
/// one is the writer's work in progress, not corruption.
#[test]
fn uncommitted_trailing_frames_are_not_read_or_refused() {
    isolated("uncommitted_frames");
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn uncommitted_frames() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let mut writer = start("uncommitted");
    let wal_path = writer.home.join("state_5.sqlite-wal");
    let mut wal = fs::read(&wal_path).unwrap();
    let page = u32::from_be_bytes(wal[8..12].try_into().unwrap()) as usize;
    let commits: Vec<bool> = wal[32..]
        .chunks_exact(24 + page)
        .map(|frame| frame[4..8] != [0; 4])
        .collect();
    // Committed frames, then the open transaction's frames.
    assert!(
        commits.contains(&true) && commits.last() == Some(&false),
        "{commits:?}"
    );
    // The last frame torn, as a reader may find a frame still being written.
    let last = wal.len() - page;
    wal[last] ^= 1;
    fs::write(&wal_path, wal).unwrap();
    let directories = directories();
    let paths = rollout_paths(&writer.home, &[CHILD_ID.into()], until()).unwrap();
    assert_eq!(
        paths[CHILD_ID],
        writer.home.join("rollout-child.jsonl").to_string_lossy()
    );
    assert_removed(&directories);
    writer.finish();
}

fn refuse_with_hook(case: &str, mut mutate: impl FnMut(&Path) + 'static) {
    let mut writer = start("positive");
    let home = writer.home.clone();
    let directories = directories();
    AFTER_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || mutate(&home)));
    });
    let error = rollout_paths(&writer.home, &[CHILD_ID.into()], until()).unwrap_err();
    AFTER_READ.with(|hook| {
        hook.borrow_mut().take();
    });
    assert!(!error.is_empty(), "{case}");
    assert_removed(&directories);
    writer.finish();
}

#[test]
fn replaced_stale_and_eof_sources_refuse() {
    isolated("replaced_source");
    isolated("stale_source");
    isolated("eof_source");
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn replaced_source() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    refuse_with_hook("replaced", |home| {
        let database = home.join("state_5.sqlite");
        let old = home.join("old.sqlite");
        fs::rename(&database, &old).unwrap();
        fs::copy(&old, &database).unwrap();
    });
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn stale_source() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    refuse_with_hook("stale", |home| {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(home.join("state_5.sqlite-wal"))
            .unwrap();
        file.write_all(&[0]).unwrap();
    });
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn eof_source() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    refuse_with_hook("eof", |home| {
        let wal = home.join("state_5.sqlite-wal");
        let length = wal.metadata().unwrap().len();
        fs::OpenOptions::new()
            .write(true)
            .open(&wal)
            .unwrap()
            .set_len(length / 2)
            .unwrap();
    });
}

#[test]
fn deadline_and_over_bound_refuse_before_copying() {
    isolated("deadline_over_bound");
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn deadline_over_bound() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    RAW_OPENS.store(0, std::sync::atomic::Ordering::Relaxed);
    let error = rollout_paths(directory.path(), &[CHILD_ID.into()], Instant::now()).unwrap_err();
    assert!(error.contains("deadline"), "{error}");
    assert_eq!(RAW_OPENS.load(std::sync::atomic::Ordering::Relaxed), 0);
    let database = directory.path().join("state_5.sqlite");
    fs::write(&database, b"SQLite format 3\0").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&database)
        .unwrap()
        .set_len(MAX_SOURCE + 1)
        .unwrap();
    let error = rollout_paths(directory.path(), &[CHILD_ID.into()], until()).unwrap_err();
    assert!(error.contains("byte bound"), "{error}");
}

#[test]
fn a_panic_removes_the_private_copy() {
    isolated("panic_cleanup");
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn panic_cleanup() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state_5.sqlite");
    let db = Connection::open(&database).unwrap();
    db.execute_batch(
        "PRAGMA journal_mode=WAL; CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT)",
    )
    .unwrap();
    drop(db);
    let directories = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&directories);
    AFTER_DIRECTORY.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |path| {
            captured.borrow_mut().push(path.to_path_buf());
            panic!("fixture panic");
        }));
    });
    let result = std::panic::catch_unwind(|| {
        let _ = rollout_paths(directory.path(), &[CHILD_ID.into()], until());
    });
    assert!(result.is_err());
    assert_removed(&directories);
}

#[test]
fn a_corrupt_wal_frame_is_refused() {
    isolated("corrupt_wal_frame");
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn corrupt_wal_frame() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let mut writer = start("positive");
    let wal_path = writer.home.join("state_5.sqlite-wal");
    let mut wal = fs::read(&wal_path).unwrap();
    assert!(wal.len() > 56);
    wal[56] ^= 1;
    fs::write(&wal_path, wal).unwrap();
    let directories = directories();
    let error = rollout_paths(&writer.home, &[CHILD_ID.into()], until()).unwrap_err();
    assert!(error.contains("checksum"), "{error}");
    assert!(directories.borrow().is_empty());
    AFTER_DIRECTORY.with(|hook| {
        hook.borrow_mut().take();
    });
    writer.finish();
}

#[test]
fn a_missing_child_is_not_evidence() {
    isolated("missing_child");
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn missing_child() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let mut writer = start("positive");
    let directories = directories();
    let error = rollout_paths(&writer.home, &[OTHER_ID.into()], until()).unwrap_err();
    assert!(error.contains("absent"), "{error}");
    assert_removed(&directories);
    writer.finish();
}

#[test]
fn rollback_mode_and_hot_journals_refuse() {
    isolated("rollback_mode_main");
    isolated("hot_journal_present");
    isolated("hot_journal_appears");
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn rollback_mode_main() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state_5.sqlite");
    let db = Connection::open(&database).unwrap();
    db.execute_batch(
        "PRAGMA journal_mode=DELETE; CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT);",
    )
    .unwrap();
    db.execute(
        "INSERT INTO threads VALUES(?1,?2)",
        rusqlite::params![CHILD_ID, "/private/fixture/committed.jsonl"],
    )
    .unwrap();
    drop(db);
    let main = fs::read(&database).unwrap();
    assert_ne!(&main[18..20], [2, 2]);
    assert!(!directory.path().join("state_5.sqlite-journal").exists());
    let directories = directories();
    let error = rollout_paths(directory.path(), &[CHILD_ID.into()], until()).unwrap_err();
    assert!(error.contains("not in WAL mode"), "{error}");
    assert!(directories.borrow().is_empty());
    AFTER_DIRECTORY.with(|hook| {
        hook.borrow_mut().take();
    });
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn hot_journal_present() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let mut writer = start("positive");
    fs::write(writer.home.join("state_5.sqlite-journal"), b"hot fixture").unwrap();
    let directories = directories();
    let error = rollout_paths(&writer.home, &[CHILD_ID.into()], until()).unwrap_err();
    assert!(error.contains("rollback journal"), "{error}");
    assert!(directories.borrow().is_empty());
    AFTER_DIRECTORY.with(|hook| {
        hook.borrow_mut().take();
    });
    writer.finish();
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn hot_journal_appears() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let mut writer = start("positive");
    let home = writer.home.clone();
    let directories = directories();
    AFTER_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            fs::write(home.join("state_5.sqlite-journal"), b"hot fixture").unwrap();
        }));
    });
    let error = rollout_paths(&writer.home, &[CHILD_ID.into()], until()).unwrap_err();
    AFTER_READ.with(|hook| {
        hook.borrow_mut().take();
    });
    assert!(error.contains("appeared"), "{error}");
    assert_removed(&directories);
    writer.finish();
}

#[test]
fn a_large_wal_honors_the_owner_deadline() {
    isolated("large_wal_deadline");
}

#[test]
#[ignore = "process-isolated private state fixture"]
fn large_wal_deadline() {
    if std::env::var("ZERUS_CODEX_SQLITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let database = home.join("state_5.sqlite");
    let wal_path = home.join("state_5.sqlite-wal");
    let db = Connection::open(&database).unwrap();
    db.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=OFF; PRAGMA wal_autocheckpoint=0;
        CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT);",
    )
    .unwrap();
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    db.execute(
        "INSERT INTO threads VALUES(?1,?2)",
        rusqlite::params![CHILD_ID, "/private/fixture/current.jsonl"],
    )
    .unwrap();
    let main = fs::read(&database).unwrap();
    for index in 0..7900 {
        db.execute(
            "UPDATE threads SET rollout_path=?1",
            rusqlite::params![format!("/private/fixture/current{}.jsonl", index % 2)],
        )
        .unwrap();
    }
    let wal = fs::read(&wal_path).unwrap();
    assert!(wal.len() > 30 * 1024 * 1024 && wal.len() <= MAX_SOURCE as usize);
    drop(db);
    fs::write(&database, main).unwrap();
    fs::write(&wal_path, wal).unwrap();
    let start = Instant::now();
    let error =
        rollout_paths(home, &[CHILD_ID.into()], start + Duration::from_millis(50)).unwrap_err();
    let elapsed = start.elapsed();
    println!(
        "large_wal_deadline_elapsed_ms={}",
        elapsed.as_secs_f64() * 1000.0
    );
    assert!(error.contains("deadline"), "{error}");
    assert!(elapsed < Duration::from_millis(150), "{elapsed:?}");
}
