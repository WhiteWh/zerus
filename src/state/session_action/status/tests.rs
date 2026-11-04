use super::*;

const NAME: &str = "codex/example/source";
const ID: &str = "11111111-1111-4111-8111-111111111111";
const ARCHIVE: &str = "22222222-2222-4222-8222-222222222222";

fn request() -> Request {
    serde_json::from_value(json!({"request_id":ID,"action":"archive",
        "expected_run_id":"run-1","expected_conversation_id":"conversation-1"}))
    .unwrap()
}
fn completed(request: &Request) -> Value {
    let mut value = receipt(request, NAME, &request_hash(request, NAME), "completed");
    value["result_target"] = json!({"name":NAME,"run_id":request.expected_run_id,
        "conversation_id":request.expected_conversation_id,"archive_id":ARCHIVE});
    value
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        // macOS's ordinary temporary prefix can itself contain a symlink.
        // Select the real owned fixture root before asking the strict reader.
        let root = fs::canonicalize(temp.path()).unwrap().join("state");
        crate::platform::private_dir(&root.join("action_receipts")).unwrap();
        Self { _temp: temp, root }
    }
    fn path(&self) -> PathBuf {
        self.root.join("action_receipts").join(format!("{ID}.json"))
    }
    fn write(&self, value: &Value) {
        atomic(&self.path(), &value.to_string()).unwrap();
    }
    fn observe(&self) -> Result<Value> {
        observe(&self.root, &request(), NAME)
    }
}

#[test]
fn absent_root_directory_and_receipt_remain_absent() {
    let f = Fixture::new();
    for root in [f.root.join("absent"), f.root.clone()] {
        assert_eq!(
            observe(&root, &request(), NAME).unwrap()["status"],
            "unavailable"
        );
        assert!(!root.join(".lock").exists());
    }
    assert!(!f.root.join("absent").exists());
    fs::remove_dir(f.root.join("action_receipts")).unwrap();
    assert_eq!(f.observe().unwrap()["status"], "unavailable");
    assert!(!f.root.join("action_receipts").exists());
}

#[test]
fn claimed_is_observed_unknown_without_rewriting_and_a_later_result_is_visible() {
    let f = Fixture::new();
    let r = request();
    f.write(&receipt(&r, NAME, &request_hash(&r, NAME), "claimed"));
    let bytes = fs::read(f.path()).unwrap();
    let modified = fs::metadata(f.path()).unwrap().modified().unwrap();
    for _ in 0..3 {
        let observed = f.observe().unwrap();
        assert_eq!(observed["status"], "uncertain");
        assert!(observed.get("request_hash").is_none());
        assert_eq!(fs::read(f.path()).unwrap(), bytes);
        assert_eq!(
            fs::metadata(f.path()).unwrap().modified().unwrap(),
            modified
        );
    }
    f.write(&completed(&r));
    assert_eq!(f.observe().unwrap()["status"], "completed");
    assert_eq!(
        fs::read_dir(f.root.join("action_receipts"))
            .unwrap()
            .count(),
        1
    );
    assert!(!f.root.join(".lock").exists());
}

#[test]
fn every_original_request_field_and_name_share_the_writers_digest() {
    let f = Fixture::new();
    let r = request();
    f.write(&completed(&r));
    for (key, value) in [
        ("action", json!("terminate")),
        ("expected_run_id", json!("other")),
        ("expected_conversation_id", json!("other")),
        ("request_id", json!(ARCHIVE)),
    ] {
        let mut changed = serde_json::to_value(&r).unwrap();
        changed[key] = value;
        let changed: Request = serde_json::from_value(changed).unwrap();
        assert!(
            project(&fs::read(f.path()).unwrap(), &changed, NAME).is_err(),
            "{key}"
        );
    }
    assert!(project(&fs::read(f.path()).unwrap(), &r, "codex/example/reused").is_err());
    // A rename argument is also part of the original writer's canonical hash.
    let mut rename: Request = serde_json::from_value(json!({"request_id":ID,"action":"rename",
        "expected_run_id":"run-1","expected_conversation_id":"conversation-1","new_name":"codex/example/new"})).unwrap();
    let value = receipt(&rename, NAME, &request_hash(&rename, NAME), "claimed");
    rename.new_name = Some("codex/example/different".into());
    assert!(project(&serde_json::to_vec(&value).unwrap(), &rename, NAME).is_err());
}

#[test]
fn malformed_future_status_and_receipt_scope_are_refused() {
    for (key, value) in [
        ("request_id", json!(ARCHIVE)),
        ("name", json!("replacement")),
        ("run_id", json!("replacement")),
        ("conversation_id", json!("replacement")),
        ("request_hash", json!("0".repeat(64))),
        ("archive_id", json!(ARCHIVE)),
        ("status", json!("future")),
        ("future_field", json!(true)),
        ("at", json!(-1)),
        ("error", json!("x".repeat(8193))),
        ("error_source", json!("future")),
    ] {
        let mut value0 = completed(&request());
        value0[key] = value;
        assert!(
            project(&serde_json::to_vec(&value0).unwrap(), &request(), NAME).is_err(),
            "{key}"
        );
    }
    assert!(project(b"{", &request(), NAME).is_err());
}

#[test]
fn completed_results_require_exact_original_target_and_canonical_archive() {
    for (key, value) in [
        ("name", json!("replacement")),
        ("run_id", json!("replacement")),
        ("conversation_id", json!("replacement")),
        ("archive_id", json!("bad")),
        ("future_field", json!(true)),
    ] {
        let mut result = completed(&request());
        result["result_target"][key] = value;
        assert!(
            project(&serde_json::to_vec(&result).unwrap(), &request(), NAME).is_err(),
            "{key}"
        );
    }
    let mut missing = completed(&request());
    missing.as_object_mut().unwrap().remove("result_target");
    assert!(project(&serde_json::to_vec(&missing).unwrap(), &request(), NAME).is_err());
    let mut wrong_status = completed(&request());
    wrong_status["status"] = json!("failed");
    assert!(project(
        &serde_json::to_vec(&wrong_status).unwrap(),
        &request(),
        NAME
    )
    .is_err());
}

#[test]
fn exact_original_archive_scope_is_retained() {
    let mut r = request();
    r.action = "forget".into();
    r.archive_id = Some(ARCHIVE.into());
    let value = receipt(&r, NAME, &request_hash(&r, NAME), "completed");
    let response = project(&serde_json::to_vec(&value).unwrap(), &r, NAME).unwrap();
    assert_eq!(response["archive_id"], ARCHIVE);
    r.archive_id = Some(ID.into());
    assert!(project(&serde_json::to_vec(&value).unwrap(), &r, NAME).is_err());
}

#[test]
fn existing_action_result_shapes_keep_their_original_scope() {
    for (action, extra, target) in [
        (
            "pause",
            json!({}),
            json!({"name":NAME,"run_id":"run-1","conversation_id":"conversation-1"}),
        ),
        (
            "resume",
            json!({}),
            json!({"name":NAME,"run_id":"run-2","conversation_id":"conversation-1"}),
        ),
        (
            "restore",
            json!({"archive_id":ARCHIVE}),
            json!({"name":NAME,"run_id":"run-2","conversation_id":"conversation-1"}),
        ),
        (
            "rename",
            json!({"new_name":"codex/example/new"}),
            json!({"name":"codex/example/new","run_id":"run-1","conversation_id":"conversation-1"}),
        ),
        (
            "rename",
            json!({"new_name":"codex/example/new","archive_id":ARCHIVE}),
            json!({"name":"codex/example/new","run_id":"run-1","conversation_id":"conversation-1","archive_id":ARCHIVE}),
        ),
        (
            "fork",
            json!({"tag":"child"}),
            json!({"name":"codex/example/child","run_id":"run-2","conversation_id":"conversation-2"}),
        ),
        (
            "remove_queued",
            json!({"message_id":"msg_example"}),
            json!({"name":NAME,"run_id":"run-1","conversation_id":"conversation-1"}),
        ),
    ] {
        let mut r = serde_json::to_value(request()).unwrap();
        r["action"] = json!(action);
        r.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let r: Request = serde_json::from_value(r).unwrap();
        validate(&r, NAME).unwrap();
        let mut value = receipt(&r, NAME, &request_hash(&r, NAME), "completed");
        value["result_target"] = target.clone();
        let response = project(&serde_json::to_vec(&value).unwrap(), &r, NAME).unwrap();
        assert_eq!(response["result_target"], target, "{action}");
    }
}

#[test]
fn private_diagnostics_are_not_returned_in_observations_or_refusals() {
    for status in ["failed", "uncertain"] {
        let r = request();
        let mut value = receipt(&r, NAME, &request_hash(&r, NAME), status);
        value["error"] = json!("private-provider-diagnostic");
        value["error_source"] = json!("opencode");
        let response = project(&serde_json::to_vec(&value).unwrap(), &r, NAME).unwrap();
        assert_eq!(response["status"], status);
        assert!(!response.to_string().contains("private-provider"));
        assert!(response.get("error_source").is_none());
    }
}

#[test]
fn receipt_bytes_are_bounded_before_parsing_at_the_exact_limit() {
    let f = Fixture::new();
    let mut bytes = completed(&request()).to_string();
    bytes.push_str(&" ".repeat(LIMIT as usize - bytes.len()));
    atomic(&f.path(), &bytes).unwrap();
    assert_eq!(f.observe().unwrap()["status"], "completed");
    bytes.push(' ');
    atomic(&f.path(), &bytes).unwrap();
    assert_eq!(f.observe().unwrap_err(), INVALID);
}

#[test]
fn invalid_request_and_clear_context_do_not_initialize_any_store() {
    let f = Fixture::new();
    for (key, value) in [
        ("action", json!("clear_context")),
        ("request_id", json!("bad")),
        ("expected_run_id", json!("")),
        ("expected_conversation_id", json!("x\n")),
    ] {
        let mut r = serde_json::to_value(request()).unwrap();
        r[key] = value;
        let r = serde_json::from_value(r).unwrap();
        assert!(observe(&f.root.join("missing"), &r, NAME).is_err());
        assert!(!f.root.join("missing").exists());
    }
}

#[cfg(unix)]
#[test]
fn links_special_files_and_nonprivate_roots_are_refused_without_changes() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let f = Fixture::new();
    f.write(&completed(&request()));
    let original = f.root.join("original.json");
    fs::rename(f.path(), &original).unwrap();
    symlink(&original, f.path()).unwrap();
    assert_eq!(f.observe().unwrap_err(), INVALID);
    fs::remove_file(f.path()).unwrap();
    fs::hard_link(&original, f.path()).unwrap();
    assert_eq!(f.observe().unwrap_err(), INVALID);
    fs::remove_file(f.path()).unwrap();
    fs::create_dir(f.path()).unwrap();
    assert_eq!(f.observe().unwrap_err(), INVALID);
    fs::remove_dir(f.path()).unwrap();
    let c = std::ffi::CString::new(f.path().to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert_eq!(f.observe().unwrap_err(), INVALID);
    fs::remove_file(f.path()).unwrap();
    fs::rename(&original, f.path()).unwrap();
    fs::set_permissions(f.path(), fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(f.observe().unwrap_err(), INVALID);
    fs::set_permissions(f.path(), fs::Permissions::from_mode(0o600)).unwrap();
    for path in [f.root.clone(), f.root.join("action_receipts")] {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(f.observe().unwrap_err(), INVALID);
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert_eq!(f.observe().unwrap()["status"], "completed");
}

#[cfg(unix)]
#[test]
fn a_symlink_in_any_existing_ancestor_never_redirects_the_receipt_read() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.write(&completed(&request()));
    let alias = f.root.parent().unwrap().join("alias");
    symlink(&f.root, &alias).unwrap();
    assert_eq!(observe(&alias, &request(), NAME).unwrap_err(), INVALID);
    let nested = f.root.join("nested");
    fs::create_dir(&nested).unwrap();
    symlink(f.root.parent().unwrap(), nested.join("redirect")).unwrap();
    let redirected = nested.join("redirect/state");
    assert_eq!(observe(&redirected, &request(), NAME).unwrap_err(), INVALID);
    assert_eq!(f.observe().unwrap()["status"], "completed");
}

#[cfg(target_os = "linux")]
#[test]
fn a_terminal_refusal_never_acquires_a_controlling_terminal() {
    use std::os::{fd::FromRawFd, unix::process::CommandExt};
    const PROBE: &str = "HGS_TEST_RECEIPT_STATUS_TTY";
    if let Some(path) = std::env::var_os(PROBE) {
        let controlling_terminal = || {
            let fd = unsafe {
                libc::open(
                    c"/dev/tty".as_ptr(),
                    libc::O_RDONLY | libc::O_NOCTTY | libc::O_NONBLOCK,
                )
            };
            if fd < 0 {
                false
            } else {
                unsafe { libc::close(fd) };
                true
            }
        };
        assert!(
            !controlling_terminal(),
            "fixture must start without a controlling terminal"
        );
        let path = PathBuf::from(path);
        let directory = Directory::open(path.parent().unwrap()).unwrap();
        assert!(directory
            .read_private(path.file_name().unwrap().to_str().unwrap())
            .is_err());
        assert!(
            !controlling_terminal(),
            "rejecting a terminal must not adopt it before the metadata check"
        );
        return;
    }
    // A new private PTY, with no native session, shell or owner terminal.
    let fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    assert!(fd >= 0);
    let _master = unsafe { fs::File::from_raw_fd(fd) };
    assert_eq!(unsafe { libc::grantpt(fd) }, 0);
    assert_eq!(unsafe { libc::unlockpt(fd) }, 0);
    let mut name = [0i8; 256];
    assert_eq!(
        unsafe { libc::ptsname_r(fd, name.as_mut_ptr(), name.len()) },
        0
    );
    let name = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
        .to_str()
        .unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "state::session_action::status::tests::a_terminal_refusal_never_acquires_a_controlling_terminal", "--nocapture"])
        .env(PROBE, name);
    // SAFETY: setsid is async-signal-safe, and only the new fixture child changes session.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let output = command.output().unwrap();
    println!(
        "private PTY child:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "private terminal-refusal child failed"
    );
}
