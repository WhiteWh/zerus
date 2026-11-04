use super::*;
use zerus_desktop_migration::canonical;

struct Fixture {
    _temp: tempfile::TempDir,
    location: Location,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let location = Location {
            root: canonical(&temp.path().join("state/desktop")).unwrap(),
            configuration: "a".repeat(64),
        };
        Self {
            _temp: temp,
            location,
        }
    }
    fn target(&self) -> std::path::PathBuf {
        self.location.root.join("imports").join(id())
    }
}
fn id() -> &'static str {
    "11111111-1111-4111-8111-111111111111"
}
fn start() -> Value {
    json!({"abi":ABI,"configuration":"a".repeat(64),"source":SOURCE,"replay":false,"import_id":id(),"max_bytes":1<<20})
}
fn item(output: &mut Vec<u8>, header: Value, body: &[u8]) {
    output.extend(serde_json::to_vec(&header).unwrap());
    output.push(b'\n');
    output.extend(body);
    output.push(b'\n');
}
pub(super) fn bundle(change: &str) -> Vec<u8> {
    let mut output = serde_json::to_vec(&start()).unwrap();
    output.push(b'\n');
    let snapshot: &[u8] = if change == "json" {
        b"x"
    } else {
        br#"{"private":"fixture body"}"#
    };
    let snapshot_ref = json!({"sha256":sha256(snapshot),"bytes":snapshot.len()});
    let metadata = json!({"version":1,"family":"settings","schema":1,"source_sha256":"b".repeat(64),"index":if change=="index" {1}else{0},"snapshot":snapshot_ref});
    let page = serde_json::to_vec(&metadata).unwrap();
    let page_ref = json!({"sha256":sha256(&page),"bytes":page.len()});
    let mut record = json!({"version":1,"configuration":"a".repeat(64),"source":SOURCE,"import_id":id(),"evidence":"staged","created_at":1,
        "automatic_approvals":false,"sources":[{"family":"settings","schema":1,"format":"json","sha256":"b".repeat(64),"build":null}],"pages":[page_ref.clone()]});
    match change {
        "drained" => record["evidence"] = "drained".into(),
        "consent" => record["automatic_approvals"] = true.into(),
        "source" => record["sources"][0]["sha256"] = "c".repeat(64).into(),
        "configuration" => record["configuration"] = "c".repeat(64).into(),
        "membership" => record["pages"] = json!([]),
        _ => (),
    }
    if change != "missing_snapshot" {
        item(
            &mut output,
            json!({"t":"snapshot","reference":snapshot_ref}),
            if change == "digest" { b"x" } else { snapshot },
        );
    }
    if change == "unused_snapshot" {
        item(
            &mut output,
            json!({"t":"snapshot","reference":{"sha256":sha256(b"null"),"bytes":4}}),
            b"null",
        );
    }
    item(&mut output, json!({"t":"page","reference":page_ref}), &page);
    if change != "missing_record" {
        let bytes = serde_json::to_vec(&record).unwrap();
        item(
            &mut output,
            json!({"t":"record","bytes":bytes.len()}),
            &bytes,
        );
    }
    if change == "trailing" {
        output.push(1);
    }
    if change == "truncated" {
        output.truncate(output.len() - 3);
    }
    output
}
#[test]
fn staged_publication_is_private_immutable_and_never_creates_authority() {
    let f = Fixture::new();
    let result = publish(&f.location, &mut &bundle("")[..]).unwrap();
    assert_eq!(result["published"], true);
    assert_eq!(result["evidence"], "staged");
    for name in ["owner.lock", "state.sqlite3", "handoff-v1.json"] {
        assert!(!f.location.root.join(name).exists());
    }
    let record = f.target().join("record-v1.json");
    let bytes = fs::read(&record).unwrap();
    assert!(platform::verify_private(&record, platform::Private::File).unwrap());
    assert!(platform::verify_private(&f.target(), platform::Private::Directory).unwrap());
    assert_eq!(result["import_record"]["sha256"], sha256(&bytes));
    assert!(publish(&f.location, &mut &bundle("")[..]).is_err());
    assert_eq!(fs::read(record).unwrap(), bytes);
    assert_eq!(
        fs::read_dir(f.location.root.join("imports"))
            .unwrap()
            .count(),
        1
    );
}
#[test]
fn incompatible_start_does_not_create_state() {
    for (key, value) in [
        ("abi", json!("future")),
        ("configuration", json!("b".repeat(64))),
        ("source", json!("dev.example")),
        ("replay", json!(true)),
        ("import_id", json!("../escape")),
        ("max_bytes", json!(0)),
        ("max_bytes", json!(MAX_BYTES + 1)),
    ] {
        let f = Fixture::new();
        let mut hello = start();
        hello[key] = value;
        let mut bytes = serde_json::to_vec(&hello).unwrap();
        bytes.push(b'\n');
        assert!(publish(&f.location, &mut &bytes[..]).is_err());
        assert!(!f.location.root.exists());
    }
}
#[test]
fn malformed_truncated_extra_and_mismatched_evidence_never_commits() {
    for change in [
        "digest",
        "json",
        "drained",
        "consent",
        "source",
        "configuration",
        "membership",
        "missing_snapshot",
        "missing_record",
        "unused_snapshot",
        "index",
        "trailing",
        "truncated",
    ] {
        let f = Fixture::new();
        assert!(
            publish(&f.location, &mut &bundle(change)[..]).is_err(),
            "{change}"
        );
        assert!(!f.target().exists(), "{change}");
        assert_eq!(
            fs::read_dir(f.location.root.join("imports"))
                .unwrap()
                .count(),
            0,
            "{change}"
        );
    }
}
#[test]
fn byte_budget_and_headers_refuse_before_body_allocation() {
    let f = Fixture::new();
    let mut hello = start();
    hello["max_bytes"] = 1.into();
    let bytes = bundle("");
    let rest = bytes.iter().position(|b| *b == b'\n').unwrap() + 1;
    let mut short = serde_json::to_vec(&hello).unwrap();
    short.push(b'\n');
    short.extend(&bytes[rest..]);
    assert!(publish(&f.location, &mut &short[..]).is_err());
    assert!(!f.target().exists());
    let f = Fixture::new();
    let mut bytes = serde_json::to_vec(&start()).unwrap();
    bytes.push(b'\n');
    bytes.extend(vec![b'x'; HEADER + 1]);
    assert!(publish(&f.location, &mut &bytes[..]).is_err());
    assert!(!f.target().exists());
    let mut budget = MAX_BYTES;
    assert!(body(
        &mut &b""[..],
        MAX_SNAPSHOT_BYTES + 1,
        MAX_SNAPSHOT_BYTES,
        &mut budget
    )
    .is_err());
}
#[test]
fn slow_input_holds_no_handoff_lock_and_partial_uuid_is_never_reused() {
    let f = Fixture::new();
    f.location.prepare().unwrap();
    let lock = f.location.handoff_lock().unwrap();
    let bytes = bundle("");
    let mut reader = io::Cursor::new(&bytes);
    let refused = publish(&f.location, &mut reader).unwrap();
    assert_eq!(refused["published"], false);
    assert_eq!(refused["code"], "handoff_busy");
    assert_eq!(refused["this_attempt_not_committed"], true);
    assert_eq!(reader.position(), bytes.len() as u64);
    assert!(!f.target().exists());
    drop(lock);
    directory(&f.target()).unwrap();
    let saved = f.target().join("partial");
    private_write(&saved, b"preserved").unwrap();
    assert_eq!(status(&f.location, id()).unwrap()["record_present"], false);
    assert!(publish(&f.location, &mut &bundle("")[..]).is_err());
    assert_eq!(fs::read(saved).unwrap(), b"preserved");
}

#[test]
fn only_actual_pre_reservation_contention_allows_a_fresh_same_record_attempt() {
    let f = Fixture::new();
    f.location.prepare().unwrap();
    let held = f.location.handoff_lock().unwrap();
    let original_error = f.location.handoff_lock().err().unwrap();
    assert!(
        original_error
            .downcast_ref::<io::Error>()
            .is_some_and(platform::lock_contended),
        "existing handoff callers retain the original OS contention error"
    );
    let refused = publish(&f.location, &mut &bundle("")[..]).unwrap();
    assert_eq!(refused["configuration"], f.location.configuration);
    assert_eq!(refused["import_id"], id());
    assert!(!f.target().exists());
    drop(held);
    let committed = publish(&f.location, &mut &bundle("")[..]).unwrap();
    assert_eq!(committed["published"], true);
    assert_eq!(committed["import_record"], refused["import_record"]);
    assert!(publish(&f.location, &mut &bundle("")[..]).is_err());
}

#[test]
fn any_failure_after_uuid_reservation_stays_unknown_even_when_status_is_absent() {
    let f = Fixture::new();
    assert!(publish_with(&f.location, &mut &bundle("")[..], |_| {
        anyhow::bail!("synthetic post-reservation failure")
    })
    .is_err());
    assert!(f.target().is_dir());
    assert_eq!(status(&f.location, id()).unwrap()["record_present"], false);
    let held = f.location.handoff_lock().unwrap();
    assert!(publish(&f.location, &mut &bundle("")[..]).is_err());
    drop(held);
    assert!(publish(&f.location, &mut &bundle("")[..]).is_err());
}

#[test]
fn path_or_open_failure_is_not_a_typed_lock_contention() {
    let f = Fixture::new();
    f.location.prepare().unwrap();
    fs::create_dir(f.location.root.join(zerus_desktop_migration::HANDOFF_LOCK)).unwrap();
    assert!(publish(&f.location, &mut &bundle("")[..]).is_err());
    assert!(!f.target().exists());
}
#[test]
#[cfg(unix)]
fn unsafe_import_directory_is_preserved_without_writing_through_it() {
    let f = Fixture::new();
    f.location.prepare().unwrap();
    let foreign = f.location.root.join("foreign");
    directory(&foreign).unwrap();
    let imports = f.location.root.join("imports");
    std::os::unix::fs::symlink(&foreign, &imports).unwrap();
    assert!(publish(&f.location, &mut &bundle("")[..]).is_err());
    assert!(imports.is_symlink());
    assert_eq!(fs::read_dir(foreign).unwrap().count(), 0);
}
#[test]
fn input_idle_and_total_deadlines_are_enforced_without_unbounded_queues() {
    let (sender, receiver) = mpsc::sync_channel(1);
    let mut input = Input {
        receiver,
        current: io::Cursor::new(Vec::new()),
        deadline: Instant::now() + Duration::from_secs(1),
        idle: Duration::from_millis(1),
        ended: false,
    };
    assert_eq!(
        input.read(&mut [0]).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    sender.send(Ok(b"x".to_vec())).unwrap();
    assert_eq!(input.read(&mut [0]).unwrap(), 1);
    input.deadline = Instant::now();
    assert_eq!(
        input.read(&mut [0]).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
}
