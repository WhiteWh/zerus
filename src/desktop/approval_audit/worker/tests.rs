use super::*;
use std::{thread, time::Duration};

struct Fixture {
    _temp: tempfile::TempDir,
    location: Location,
    owner: Arc<platform::FileLock>,
    worker: Arc<Worker>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().canonicalize().unwrap().join("state");
        let location = Location {
            root: state.join("desktop"),
            configuration: "a".repeat(64),
        };
        location.prepare().unwrap();
        let owner = Arc::new(location.lock().unwrap());
        let reader = Reader {
            root: state.join("approval-audit"),
            configuration: location.configuration.clone(),
            epoch: uuid::Uuid::new_v4().to_string(),
            journal: None,
        };
        let worker = Arc::new(Worker::from_reader(
            reader,
            Arc::new(AtomicBool::new(false)),
        ));
        Self {
            _temp: temp,
            location,
            owner,
            worker,
        }
    }
    fn query(&self) -> Value {
        json!({"op":"approval_audit_list","request_id":uuid::Uuid::new_v4().to_string(),
            "configuration":self.worker.configuration,"epoch":self.worker.epoch})
    }
    fn hello(&self, value: Value) -> Hello {
        Hello {
            abi: ABI.into(),
            configuration: self.worker.configuration.clone(),
            op: "request".into(),
            request: Some(value),
        }
    }
    fn admit(&self, request: Value) -> Reply {
        self.worker
            .admit(self.hello(request), self.owner.clone(), 19)
    }
}

#[test]
fn actor_admission_never_waits_for_the_reader_and_has_one_original_slot() {
    let f = Fixture::new();
    let guard = f.worker.reader.lock().unwrap();
    let worker = f.worker.clone();
    let owner = f.owner.clone();
    let hello = f.hello(f.query());
    let (send, receive) = mpsc::sync_channel(1);
    let admission = thread::spawn(move || {
        let reply = worker.admit(hello, owner, 19);
        send.send(reply).unwrap();
    });
    // The actor must return while the reader is deliberately unavailable.
    // Release the gate and join on every path before asserting the result.
    let reply = receive.recv_timeout(Duration::from_secs(2));
    drop(guard);
    admission.join().unwrap();
    let mut reply = reply.expect("actor admission performed reader I/O");
    let refused = f.admit(f.query()).response();
    assert_eq!(refused["value"]["code"], "approval_audit_unavailable");
    let result = reply.response();
    assert_eq!(result["epoch"], f.worker.epoch);
    assert_eq!(result["seq"], 19);
    assert_eq!(result["value"]["accepted"], true);
    // Completing SQL alone does not free original transport custody.
    assert_eq!(f.admit(f.query()).response()["value"]["accepted"], false);
    drop(reply);
    assert_eq!(f.admit(f.query()).response()["value"]["accepted"], true);
    assert!(!f
        .location
        .root
        .parent()
        .unwrap()
        .join("approval-audit")
        .exists());
}

#[test]
fn scope_and_closed_request_validation_refuse_before_reader_work() {
    let f = Fixture::new();
    let mut stale = f.query();
    stale["epoch"] = json!(uuid::Uuid::new_v4().to_string());
    assert_eq!(
        f.admit(stale).response()["value"]["code"],
        "approval_audit_scope_changed"
    );
    let mut malformed = f.query();
    malformed["extra"] = json!(true);
    assert_eq!(
        f.admit(malformed).response()["value"]["code"],
        "invalid_approval_audit_request"
    );
    let mut oversized = f.query();
    oversized["host"] = json!("x".repeat(wire::REQUEST_BYTES));
    assert_eq!(
        f.admit(oversized).response()["value"]["code"],
        "invalid_approval_audit_request"
    );
    let mut incompatible = f.hello(f.query());
    incompatible.configuration = "b".repeat(64);
    let result = f.worker.admit(incompatible, f.owner.clone(), 19).response();
    assert_eq!(result["t"], "error");
    assert_eq!(result["value"]["code"], "incompatible_configuration");
    assert!(!f.worker.busy.load(Ordering::SeqCst));
    assert!(f.worker.reader.lock().unwrap().journal.is_none());
}

#[test]
fn stopped_original_work_cannot_open_a_journal_or_publish_an_accepted_reply() {
    let f = Fixture::new();
    let mut original = f.admit(f.query());
    f.worker.stopped.store(true, Ordering::SeqCst);
    assert_eq!(
        original.response()["value"]["code"],
        "approval_audit_unavailable"
    );
    drop(original);
    assert_eq!(f.admit(f.query()).response()["value"]["accepted"], false);
    assert!(f.worker.reader.lock().unwrap().journal.is_none());
}

#[test]
fn dropped_admission_releases_only_its_original_slot() {
    let f = Fixture::new();
    let (send, receive) = mpsc::sync_channel(1);
    drop(receive);
    let result = send.try_send(f.admit(f.query()));
    assert!(result.is_err());
    assert!(f.worker.busy.load(Ordering::SeqCst));
    drop(result);
    assert!(!f.worker.busy.load(Ordering::SeqCst));
    assert!(f.worker.reader.lock().unwrap().journal.is_none());
}

#[test]
fn one_retained_reader_preserves_the_original_journal_across_client_requests() {
    let f = Fixture::new();
    let root = f.location.root.parent().unwrap().join("approval-audit");
    let db = crate::approval_log::open(&root).unwrap();
    db.execute(
        "INSERT INTO attempts(identity,request_id,at,host,session,provider,prompt,question)
        VALUES('original','original',1,'a.example','fixture','claude','question','{}')",
        [],
    )
    .unwrap();
    let mut first = f.admit(f.query());
    let value = first.response()["value"].clone();
    assert_eq!(value["accepted"], true);
    drop(first);
    let mut query = f.query();
    query["journal"] = value["journal"].clone();
    query["through"] = value["result"]["through"].clone();
    let next = f.admit(query).response();
    assert_eq!(next["value"]["journal"], value["journal"]);
    assert_eq!(next["value"]["accepted"], true);
}

#[test]
fn original_query_keeps_the_service_lock_after_actor_shutdown() {
    let f = Fixture::new();
    let root = f.location.root.parent().unwrap().join("approval-audit");
    drop(crate::approval_log::open(&root).unwrap());
    let mut reply = f.admit(f.query());
    assert_eq!(reply.response()["value"]["accepted"], true);
    let weak = Arc::downgrade(&f.worker);
    f.worker.stopped.store(true, Ordering::SeqCst);
    drop(f.worker);
    drop(f.owner);
    // A logical shutdown/deadline does not stand in for the original read,
    // SQLite connection lifetime or release the physical lock.
    assert!(f.location.lock().is_err());
    assert!(weak.upgrade().is_some());
    drop(reply);
    assert!(weak.upgrade().is_none());
    assert!(f.location.lock().is_ok());
}

#[cfg(unix)]
#[test]
fn original_transport_closes_before_the_audit_slot_is_released() {
    use std::io::Read;
    let f = Fixture::new();
    let (stream, mut peer) = transport::Stream::pair().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut session = Session {
        stream,
        reply: Some(f.admit(f.query())),
    };
    assert_eq!(
        session.reply.as_mut().unwrap().response()["value"]["accepted"],
        true
    );
    assert!(f.worker.busy.load(Ordering::SeqCst));
    drop(session);
    let mut byte = [0];
    assert_eq!(peer.read(&mut byte).unwrap(), 0);
    assert!(!f.worker.busy.load(Ordering::SeqCst));
    assert_eq!(f.admit(f.query()).response()["value"]["accepted"], true);
}

#[cfg(unix)]
#[test]
fn fifo_cannot_block_the_original_audit_reader() {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    };
    let f = Fixture::new();
    let root = f.location.root.parent().unwrap().join("approval-audit");
    platform::private_dir(&root).unwrap();
    let path = root.join("journal.sqlite3");
    let name = CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let mut reply = f.admit(f.query());
    let (send, receive) = mpsc::sync_channel(1);
    let query = thread::spawn(move || send.send(reply.response()).unwrap());
    let result = receive.recv_timeout(Duration::from_secs(2));
    // Rescue an old blocking-open implementation before asserting the bound.
    // This opens only our original private FIFO; no descendant is left alive.
    let rescue = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&path);
    drop(rescue);
    query.join().unwrap();
    assert_eq!(
        result.expect("audit FIFO open blocked")["value"]["accepted"],
        false
    );
}
