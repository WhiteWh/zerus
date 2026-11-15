use super::*;
use serde_json::json;
use std::{fs, fs::OpenOptions, path::PathBuf, process::Command, time::Duration};

const ID: &str = "00000000-0000-4000-8000-000000000001";
const RESTART_ROOT: &str = "ZERUS_TEST_START_REFUSAL_ROOT";
const RESTART: &str = "state::input::task_refusal::windows_tests::retained_refusal_restart_probe";
fn until() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn scope() -> Scope {
    Scope::new(
        "codex/example/worker",
        &json!({"request_id":ID,
        "text":"Original task prompt", "expected_run_id":"original-run",
        "expected_conversation_id":"original-conversation"}),
        &"b".repeat(64),
    )
    .unwrap()
}
struct Fixture {
    // Release the Windows directory pins before TempDir attempts removal.
    writer: writer::Writer,
    root: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let root =
            tempfile::tempdir_in(platform::canonical(std::env::temp_dir()).unwrap()).unwrap();
        platform::private_dir(root.path()).unwrap();
        platform::private_open(OpenOptions::new().create_new(true).read(true).write(true))
            .open(root.path().join(".lock"))
            .unwrap();
        let writer = writer::test_open(root.path(), until()).unwrap();
        Self { writer, root }
    }
    fn file(&self) -> PathBuf {
        self.root.path().join(FOLDER).join(scope().filename())
    }
    fn publish(&self) -> Result<Observed> {
        remember_locked(&self.writer, &scope(), Mode::Managed, Mode::Legacy)
    }
}
fn old_reader(path: &std::path::Path) {
    assert!(
        matches!(
            super::super::existing(path, &scope().digest, &scope().name, false),
            super::super::Existing::Unknown(_)
        ),
        "an unsupported retained refusal must never become absent or fresh input"
    );
}

#[test]
fn unqualified_windows_namespace_durability_emits_no_proof() {
    let f = Fixture::new();
    assert_eq!(
        lookup(&scope(), Instant::now()).unwrap(),
        Observed::Inactive
    );
    assert_eq!(
        remember(&scope(), Mode::Managed, Mode::Legacy, Instant::now()).unwrap(),
        Observed::Inactive
    );
    assert!(remember_enabled(&scope(), Mode::Managed, Mode::Legacy, until()).is_err());
    assert!(!f.root.path().join(FOLDER).exists());
    f.writer.verify().unwrap();
}

#[test]
fn file_flush_and_the_compatibility_noop_never_prove_namespace_completion() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ};
    let f = Fixture::new();
    let dir = directory(&f.writer, true).unwrap().unwrap();
    let record = Record::new(&scope(), Mode::Managed, Mode::Legacy).unwrap();
    let bytes = serde_json::to_vec(&record).unwrap();
    let original = dir.write_new_file(&scope().filename(), &bytes).unwrap();
    original.sync_all().unwrap();
    platform::sync_dir(&dir.path).unwrap(); // Compatibility only, never evidence.
    let native = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .share_mode(FILE_SHARE_READ)
        .open(&dir.path)
        .unwrap();
    let flush = native.sync_all();
    eprintln!(
        "START_REFUSAL_NATIVE file_flush=true directory_read_handle_flush_errno={:?}",
        flush.as_ref().err().and_then(std::io::Error::raw_os_error)
    );
    assert!(
        dir.sync().is_err(),
        "the original namespace completion remains unqualified"
    );
    assert!(
        lookup_locked(&f.writer, &scope()).is_err(),
        "read-back must not mint a durable refusal proof"
    );
    assert_eq!(fs::read(f.file()).unwrap(), bytes);
    old_reader(&f.file());
}

#[test]
fn failure_after_publication_keeps_the_original_uuid_unknown_and_never_replaces() {
    let f = Fixture::new();
    assert_eq!(
        lookup_locked(&f.writer, &scope()).unwrap(),
        Observed::Absent
    );
    assert!(
        f.publish().is_err(),
        "unqualified file/namespace completion must stay Unknown"
    );
    let bytes = fs::read(f.file()).unwrap();
    let dir = directory(&f.writer, false).unwrap().unwrap();
    let original = dir.read_private(&scope().filename()).unwrap();
    let identity = platform::FileIdentity::of(&original).unwrap();
    drop(original); // NOREPLACE must hold even without an original file reader.
    assert!(dir
        .write_new_file(&scope().filename(), b"must not replace")
        .is_err());
    assert_eq!(fs::read(f.file()).unwrap(), bytes);
    assert_eq!(
        platform::FileIdentity::of(&dir.read_private(&scope().filename()).unwrap()).unwrap(),
        identity
    );
    for _ in 0..2 {
        assert!(lookup_locked(&f.writer, &scope()).is_err());
        assert!(f.publish().is_err());
        assert_eq!(fs::read(f.file()).unwrap(), bytes);
    }
    old_reader(&f.file());
}

#[test]
fn existing_pending_submitted_and_damaged_targets_are_never_repaired() {
    let f = Fixture::new();
    assert!(f.publish().is_err());
    let mut cases = vec![b"{partial".to_vec(), b"null".to_vec(), b"{}".to_vec()];
    for status in ["pending", "submitted", "in_progress", "future_status"] {
        cases.push(
            serde_json::to_vec(&json!({"status":status,
            "name":scope().name,"digest":scope().digest}))
            .unwrap(),
        );
    }
    for bytes in cases {
        fs::write(f.file(), &bytes).unwrap();
        assert!(matches!(f.publish(), Err(_) | Ok(Observed::Existing)));
        assert_eq!(fs::read(f.file()).unwrap(), bytes);
        assert!(!matches!(
            super::super::existing(&f.file(), &scope().digest, &scope().name, false),
            super::super::Existing::Absent
        ));
    }
}

#[test]
fn the_original_input_writer_excludes_publication_and_the_root_stays_pinned() {
    let f = Fixture::new();
    let original = f.root.path().to_path_buf();
    let moved = original.with_extension("moved");
    assert!(fs::rename(&original, &moved).is_err());
    f.writer.verify().unwrap();
    drop(f.writer);
    fs::rename(&original, &moved).unwrap();
    fs::rename(&moved, &original).unwrap();
    let input = storage::lock(Some(&original.join(".lock"))).unwrap();
    let error = writer::test_open(&original, Instant::now() + Duration::from_secs(1))
        .err()
        .unwrap();
    assert_eq!(error, "The original native writer is busy");
    assert!(!original.join(FOLDER).exists());
    drop(input);
    let writer = writer::test_open(&original, until()).unwrap();
    assert!(remember_locked(&writer, &scope(), Mode::Managed, Mode::Legacy).is_err());
    assert!(original.join(FOLDER).join(scope().filename()).is_file());
}

#[test]
fn restart_preserves_unknown_tombstones_and_old_readers_never_offer_them() {
    let f = Fixture::new();
    assert!(f.publish().is_err());
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.file())
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(86400))
        .unwrap();
    let bytes = fs::read(f.file()).unwrap();
    let path = f.file();
    drop(f.writer);
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            RESTART,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(RESTART_ROOT, f.root.path())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("test result: ok. 1 passed; 0 failed; 0 ignored;"),
        "restart probe must execute exactly one control"
    );
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
#[ignore = "owned restart entry point, explicitly run by the native control"]
fn retained_refusal_restart_probe() {
    let root = PathBuf::from(std::env::var_os(RESTART_ROOT).unwrap());
    let writer = writer::test_open(&root, until()).unwrap();
    let file = root.join(FOLDER).join(scope().filename());
    let bytes = fs::read(&file).unwrap();
    assert!(lookup_locked(&writer, &scope()).is_err());
    assert!(remember_locked(&writer, &scope(), Mode::Legacy, Mode::Managed).is_err());
    assert_eq!(fs::read(&file).unwrap(), bytes);
    old_reader(&file);
}
