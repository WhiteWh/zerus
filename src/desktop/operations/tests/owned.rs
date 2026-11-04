//! Private synthetic process/custody mechanisms, never current G1 admission.
use super::*;
use crate::desktop::{
    custody,
    store::{Location, Store},
};
use crate::platform;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, Weak,
    },
    thread,
    time::{Duration, Instant},
};
use zerus_client_core::{
    run::{custody::InheritedLock, run_owned, OwnedError},
    Invocation,
};

const PREFIX: &str = "desktop::operations::tests::owned::";

fn wait(mut done: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(8);
    while !done() {
        assert!(
            Instant::now() < end,
            "private fixture did not reach its bounded observation"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
fn environment(command: &mut Command, root: &Path) {
    command
        .env_clear()
        .env("PATH", "/opt/homebrew/bin:/usr/bin:/bin")
        .env("LANG", "C.UTF-8");
    for (key, leaf) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_RUNTIME_DIR", "runtime"),
        ("HGS_CONFIG_DIR", "config"),
        ("HGS_STATE_DIR", "state"),
        ("TMPDIR", "tmp"),
        ("TMUX_TMPDIR", "tmux"),
    ] {
        let path = root.join(leaf);
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        command.env(key, path);
    }
}
fn isolated(case: &str) -> bool {
    let exact = format!("{PREFIX}{case}");
    if std::env::var("OWNED_PORT_CASE").as_deref() == Ok(exact.as_str()) {
        return true;
    }
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap());
    environment(&mut child, &root_path);
    let result = child
        .args([&exact, "--exact", "--nocapture", "--test-threads=1"])
        .env("OWNED_PORT_CASE", &exact)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "private exact child failed\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    print!("{}", String::from_utf8_lossy(&result.stdout));
    false
}

// Each fake executable is fixed private fixture code. Its argv/stdin are the
// unchanged Proposed call; it invokes no hgs, native agent, SSH or service.
const FAKE: &str = r#"#!/usr/bin/env python3
import json,os,pathlib,sys,time
p=pathlib.Path(os.environ['OWNED_PORT_ROOT']); mode=os.environ['OWNED_PORT_MODE']
assert 'HGS_SESSION' not in os.environ and 'HGS_RUN_ID' not in os.environ
end=time.monotonic()+6
while not (p/'start').exists():
 if time.monotonic()>=end: os._exit(81)
 time.sleep(.002)
(p/'args.json').write_text(json.dumps(sys.argv[1:]))
(p/'running').write_text('actual child after birth registration turn')
reply=(p/'reply.json').read_bytes()
if mode=='incomplete':
 os.close(0);sys.stdout.buffer.write(reply);sys.stdout.buffer.flush();os._exit(0)
held=mode in ['stdin','stdout','stderr','crash']
if mode!='stdin': (p/'input.bin').write_bytes(sys.stdin.buffer.read())
if held:
 pid=os.fork()
 if pid==0:
  keep={'stdin':0,'stdout':1,'stderr':2,'crash':1}[mode]
  for fd in range(3):
   if fd!=keep: os.close(fd)
  # Ordinary pipe controls remove inherited custody so ONLY the actual blocked
  # writer/reader's Arc can hold it. Crash control retains custody in its survivor.
  if mode!='crash':
   for fd in os.listdir('/dev/fd'):
    if int(fd)>2:
     try: os.close(int(fd))
     except OSError: pass
  (p/'descendant').write_text(str(os.getpid()))
  end=time.monotonic()+30
  while not (p/'release').exists():
   if time.monotonic()>=end: os._exit(82)
   time.sleep(.002)
  (p/'released').write_text('explicit descendant gate')
  os._exit(0)
 end=time.monotonic()+3
 while not (p/'descendant').exists():
  if time.monotonic()>=end: os._exit(83)
  time.sleep(.002)
if mode=='child':
 os.close(1);os.close(2);(p/'outputs-closed').write_text('owned child still alive')
 end=time.monotonic()+6
 while not (p/'release').exists():
  if time.monotonic()>=end: os._exit(85)
  time.sleep(.002)
 (p/'released').write_text('explicit owned-child gate')
 os._exit(0)
if mode in ['timeout','cancel']:
 end=time.monotonic()+10
 while time.monotonic()<end: time.sleep(.01)
 os._exit(84)
if mode=='oversize': reply=b'x'*1048577
if mode=='malformed': reply=b'{"schema":999}'
sys.stdout.buffer.write(reply);sys.stdout.buffer.flush()
os._exit(7 if mode=='nonzero' else 0)
"#;

struct Host {
    _temp: Option<tempfile::TempDir>,
    root: PathBuf,
    location: Location,
    fixture: Fixture,
    admitted: ledger::Admitted,
    ticket: ledger::Ticket,
    store: Arc<Mutex<Store>>,
    lock: Option<Arc<InheritedLock>>,
    weak: Weak<InheritedLock>,
    pin: Arc<Mutex<Option<Arc<InheritedLock>>>>,
    registered: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    spent: bool,
}
impl Host {
    fn new(large: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let mut host = Self::at(&temp.path().canonicalize().unwrap(), large);
        host._temp = Some(temp);
        host
    }
    fn at(root: &Path, large: bool) -> Self {
        fs::create_dir_all(root).unwrap();
        fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
        let mut f = Fixture::new();
        if large {
            // Construct a separate fixture original BEFORE admission. Never
            // replace bytes of an existing admitted operation to fill a pipe.
            let mut record: Value = serde_json::from_slice(&f.raw).unwrap();
            record["intent"]["plan"]["opaque_future"]["pipe_fixture"] =
                json!("x".repeat(512 << 10));
            f.input = serde_json::to_vec(&record["intent"]["plan"]).unwrap();
            f.raw = serde_json::to_vec(&record).unwrap();
            f.binding.source_record_sha256 = hash(&f.raw);
            f.binding.ui_snapshot_sha256 =
                zerus_client_core::tasks_write::outbox::snapshot(&record);
            f.binding.task_start_reconcile_input_v1_sha256 = hash(&f.input);
        }
        let location = Location {
            root: root.join("state/desktop"),
            configuration: f.binding.key.configuration.clone(),
        };
        location.prepare().unwrap();
        let t = f.ticket();
        let mut store = Store::open(&location, &t.service_epoch).unwrap();
        zerus_desktop_ledger::authorize(&mut store.db, &t.writer_epoch, &t.service_epoch).unwrap();
        let admitted = f.seed(&mut store.db);
        let ticket = ledger::Ticket {
            operation_id: admitted.operation_id.clone(),
            admission_id: admitted.admission_id.clone(),
            binding_sha256: admitted.binding_sha256.clone(),
            revision: admitted.revision,
            ..t
        };
        let (prepared, lock) = custody::prepare(&location, &ticket).unwrap();
        let reservation = ledger::custody::reserve(
            &mut store.db,
            &ledger::Reserve {
                operation_id: ticket.operation_id.clone(),
                attempt_id: ticket.attempt_id.clone(),
                revision: ticket.revision,
                writer_epoch: ticket.writer_epoch.clone(),
                service_epoch: ticket.service_epoch.clone(),
                lease: ticket.lease.clone(),
            },
            &prepared,
        )
        .unwrap();
        assert!(reservation.fresh);
        assert_eq!(reservation.attempt.ticket, ticket);
        fs::write(root.join("fake-hgs"), FAKE).unwrap();
        fs::set_permissions(root.join("fake-hgs"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.join("reply.json"), f.reply()).unwrap();
        let weak = Arc::downgrade(&lock);
        Self {
            _temp: None,
            root: root.into(),
            location,
            fixture: f,
            admitted,
            ticket,
            store: Arc::new(Mutex::new(store)),
            lock: Some(lock),
            weak,
            pin: Arc::default(),
            registered: Arc::default(),
            calls: Arc::default(),
            spent: false,
        }
    }
    fn run(
        &mut self,
        mode: &str,
        revoke: bool,
        refuse_registration: bool,
    ) -> Option<Result<Vec<u8>, OwnedError>> {
        if self.spent {
            return None;
        }
        self.spent = true;
        let loaded = Loaded::load(&self.store.lock().unwrap().db, &self.admitted).unwrap();
        let proposal = loaded.proposal(false).unwrap();
        let inv = Invocation {
            args: proposal.args().to_vec(),
            stdin: Some(proposal.input().to_vec()),
            timeout: Duration::from_millis(if matches!(mode, "normal" | "child") {
                2500
            } else {
                // This includes interpreter startup under a parallel native
                // harness. Holds release by explicit gates, never this delay.
                2000
            }),
            request_id: None,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let setter = if mode == "cancel" {
            let cancel = cancel.clone();
            let root = self.root.clone();
            Some(thread::spawn(move || {
                wait(|| root.join("running").exists());
                cancel.store(true, Ordering::Release);
            }))
        } else {
            None
        };
        let lock = self.lock.take().unwrap();
        // Missing registration never becomes a positive empty-resource proof,
        // including a refusal before the guarded hook returns any Child.
        *self.pin.lock().unwrap() = Some(lock.clone());
        let inherited = lock.clone();
        let root = self.root.clone();
        let child_mode = mode.to_owned();
        let store = self.store.clone();
        let ticket = self.ticket.clone();
        let registered = self.registered.clone();
        let pin = self.pin.clone();
        let calls = self.calls.clone();
        let result = run_owned(
            &self.root.join("fake-hgs"),
            &inv,
            &[],
            &cancel,
            move |command| {
                calls.fetch_add(1, Ordering::AcqRel);
                environment(command, &root.join("child-env"));
                command
                    .env("OWNED_PORT_ROOT", &root)
                    .env("OWNED_PORT_MODE", child_mode);
                inherited.inherit(command)?; // LAST hook, before this one owned fork.
                let child = command.spawn()?;
                // From here no failure can mean StartFailed/no child. Preserve its
                // actual owner, allow the runner to wait/drain, and pin unknowns.
                let bound = platform::process_identity(child.id()).is_some_and(|birth| {
                    let mut ticket = ticket.clone();
                    if refuse_registration {
                        ticket.lease = uuid::Uuid::new_v4().to_string();
                    }
                    ledger::custody::register(
                        &mut store.lock().unwrap().db,
                        &ticket,
                        &ledger::custody::Birth {
                            pid: child.id(),
                            birth: birth.birth,
                        },
                    )
                    .is_ok()
                });
                registered.store(bound, Ordering::Release);
                if !bound {
                    *pin.lock().unwrap() = Some(inherited.clone());
                }
                if bound && revoke {
                    zerus_desktop_ledger::revoke(&store.lock().unwrap().db, "fixture").unwrap();
                }
                if fs::write(root.join("start"), b"birth turn complete").is_err() {
                    registered.store(false, Ordering::Release);
                    *pin.lock().unwrap() = Some(inherited.clone());
                }
                Ok(child)
            },
            lock,
        );
        if let Some(setter) = setter {
            setter.join().unwrap();
        }
        if self.registered.load(Ordering::Acquire) {
            self.pin.lock().unwrap().take();
        }
        let result_tag = result
            .as_ref()
            .map(|_| "complete".to_owned())
            .unwrap_or_else(|error| format!("{error:?}"));
        println!(
            "OWNED EXCHANGE {mode}: launches={} registered={} result={result_tag}",
            self.calls.load(Ordering::Acquire),
            self.registered.load(Ordering::Acquire)
        );
        let mut store = self.store.lock().unwrap();
        let settled = self.registered.load(Ordering::Acquire)
            && result.as_ref().is_ok_and(|bytes| {
                loaded
                    .finish_submitted(&mut store.db, &self.ticket, bytes)
                    .is_ok()
            });
        if !settled {
            ledger::finish(
                &mut store.db,
                &self.ticket,
                &ledger::Outcome::Unknown {
                    reason: ledger::Unknown::LostResult,
                },
            )
            .unwrap();
        }
        assert_eq!(self.calls.load(Ordering::Acquire), 1);
        Some(result)
    }
    fn record(&self) -> ledger::custody::Record {
        ledger::custody::record(&self.store.lock().unwrap().db, &self.ticket)
            .unwrap()
            .unwrap()
    }
    fn unchanged(&self) {
        let store = self.store.lock().unwrap();
        let frozen = ledger::load(&store.db, &self.admitted.operation_id).unwrap();
        assert_eq!(frozen.raw_record, self.fixture.raw);
        assert_eq!(frozen.retained_input, self.fixture.input);
        assert_eq!(frozen.admitted, self.admitted);
    }
    fn held(&self) {
        held(&self.store.lock().unwrap().db);
        assert!(matches!(
            custody::probe(&self.location, &self.record()),
            custody::Observation::Held
        ));
    }
    fn recover(&self) {
        // The existing kernel probe -> actor commit is the ONLY release path.
        wait(|| {
            let mut store = self.store.lock().unwrap();
            store.retry_operation_custody();
            store.recover_operation_custody(&self.location);
            ledger::totals(&store.db).unwrap().undrained == 0
        });
    }
    fn descendant(&self) -> ledger::custody::Birth {
        let pid = fs::read_to_string(self.root.join("descendant"))
            .unwrap()
            .parse()
            .unwrap();
        let birth = platform::process_identity(pid).unwrap().birth;
        ledger::custody::Birth { pid, birth }
    }
    fn release(&self, birth: &ledger::custody::Birth) {
        fs::write(
            self.root.join("release"),
            b"release this exact synthetic descendant",
        )
        .unwrap();
        wait(|| {
            matches!(
                platform::process_state(birth.pid, Some(&birth.birth)),
                Ok(platform::ProcessState::Exited)
            )
        });
        assert_eq!(
            fs::read_to_string(self.root.join("released")).unwrap(),
            "explicit descendant gate"
        );
        wait(|| self.weak.upgrade().is_none());
        self.wait_for_kernel_release(|_| ());
    }
    fn wait_for_kernel_release(&self, mut observe: impl FnMut(&custody::Observation)) {
        // Weak::upgrade can fail before the last Arc's File destructor closes
        // its descriptor. Only the exact existing kernel lock proves release.
        wait(|| {
            let observation = custody::probe(&self.location, &self.record());
            observe(&observation);
            matches!(observation, custody::Observation::Released { .. })
        });
    }
}

#[test]
fn exact_wire_and_registered_old_epoch_receipt_settle_without_reissuing() {
    if !isolated("exact_wire_and_registered_old_epoch_receipt_settle_without_reissuing") {
        return;
    }
    for revoke in [false, true] {
        let mut host = Host::new(false);
        assert!(host.run("normal", revoke, false).unwrap().is_ok());
        let args: Vec<String> =
            serde_json::from_slice(&fs::read(host.root.join("args.json")).unwrap()).unwrap();
        assert_eq!(
            args,
            Loaded::load(&host.store.lock().unwrap().db, &host.admitted)
                .unwrap()
                .proposal(false)
                .unwrap()
                .args()
        );
        assert_eq!(
            fs::read(host.root.join("input.bin")).unwrap(),
            host.fixture.input
        );
        assert_eq!(attempt(&host.store.lock().unwrap().db).0, "submitted");
        assert!(host.run("normal", false, false).is_none());
        assert_eq!(host.calls.load(Ordering::Acquire), 1);
        wait(|| host.weak.upgrade().is_none());
        held(&host.store.lock().unwrap().db); // A receipt is never drain proof.
        host.unchanged();
        host.recover();
        host.unchanged();
    }
}

#[test]
fn invalid_or_incomplete_owned_exchange_never_settles_a_plausible_receipt() {
    if !isolated("invalid_or_incomplete_owned_exchange_never_settles_a_plausible_receipt") {
        return;
    }
    for mode in [
        "nonzero",
        "malformed",
        "oversize",
        "incomplete",
        "timeout",
        "cancel",
    ] {
        let mut host = Host::new(mode == "incomplete");
        let result = host.run(mode, false, false).unwrap();
        if mode == "malformed" {
            assert!(result.is_ok());
        } else {
            assert!(result.is_err());
        }
        assert_eq!(attempt(&host.store.lock().unwrap().db).0, "unknown");
        held(&host.store.lock().unwrap().db);
        assert!(host.run("normal", false, false).is_none());
        wait(|| host.weak.upgrade().is_none());
        host.recover();
        host.unchanged();
        assert_eq!(attempt(&host.store.lock().unwrap().db).0, "unknown");
    }
}

#[test]
fn child_exit_is_required_after_input_and_both_outputs_have_ended() {
    if !isolated("child_exit_is_required_after_input_and_both_outputs_have_ended") {
        return;
    }
    let mut host = Host::new(false);
    let store = host.store.clone();
    let root = host.root.clone();
    let ticket = host.ticket.clone();
    let location = Location {
        root: host.location.root.clone(),
        configuration: host.location.configuration.clone(),
    };
    let running = thread::spawn(move || {
        host.run("child", false, false);
        host
    });
    wait(|| root.join("outputs-closed").exists());
    let record = ledger::custody::record(&store.lock().unwrap().db, &ticket)
        .unwrap()
        .unwrap();
    let birth = record.helper.as_ref().unwrap();
    assert!(matches!(
        platform::process_state(birth.pid, Some(&birth.birth)),
        Ok(platform::ProcessState::Running)
    ));
    held(&store.lock().unwrap().db);
    assert!(!running.is_finished());
    assert!(matches!(
        custody::probe(&location, &record),
        custody::Observation::Held
    ));
    fs::write(root.join("release"), b"release this owned child").unwrap();
    let host = running.join().unwrap();
    assert_eq!(
        fs::read_to_string(root.join("released")).unwrap(),
        "explicit owned-child gate"
    );
    wait(|| host.weak.upgrade().is_none());
    assert_eq!(attempt(&host.store.lock().unwrap().db).0, "unknown"); // Empty output cannot settle.
    host.recover();
    host.unchanged();
}

#[test]
fn each_real_last_pipe_keeps_custody_and_sql_after_the_runner_returns() {
    if !isolated("each_real_last_pipe_keeps_custody_and_sql_after_the_runner_returns") {
        return;
    }
    for mode in ["stdin", "stdout", "stderr"] {
        let mut host = Host::new(mode == "stdin");
        assert!(host.run(mode, false, false).unwrap().is_err());
        let descendant = host.descendant();
        assert!(
            host.weak.upgrade().is_some(),
            "actual {mode} resource guard must survive"
        );
        host.held();
        host.unchanged();
        {
            let mut store = host.store.lock().unwrap();
            store.retry_operation_custody();
            store.recover_operation_custody(&host.location);
        }
        host.held();
        host.release(&descendant);
        host.recover();
        host.unchanged();
        assert_eq!(attempt(&host.store.lock().unwrap().db).0, "unknown");
        println!("OWNED PIPE {mode}: actual last resource, exact descendant exit and kernel/SQL release; Unknown unchanged");
    }
}

#[test]
fn a_released_weak_reference_still_waits_for_the_exact_kernel_descriptor() {
    if !isolated("a_released_weak_reference_still_waits_for_the_exact_kernel_descriptor") {
        return;
    }
    use std::os::fd::{AsRawFd, FromRawFd};
    let mut host = Host::new(false);
    // Duplicate the SAME open-file description without keeping its Arc alive.
    // This makes the last-Arc -> descriptor-close ordering deterministic.
    // SAFETY: the live inherited-lock descriptor and duplication arguments are valid.
    let fd = unsafe {
        libc::fcntl(
            host.lock.as_ref().unwrap().as_raw_fd(),
            libc::F_DUPFD_CLOEXEC,
            3,
        )
    };
    assert!(fd >= 0);
    // SAFETY: fcntl returned this test's one new owned descriptor.
    let retained = unsafe { fs::File::from_raw_fd(fd) };
    assert!(host.run("normal", false, false).unwrap().is_ok());
    wait(|| host.weak.upgrade().is_none());
    host.held();
    let (observed, receive) = std::sync::mpsc::sync_channel(1);
    let (release, wait_release) = std::sync::mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let mut first = true;
        host.wait_for_kernel_release(|observation| {
            if first {
                assert!(matches!(observation, custody::Observation::Held));
                observed.send(()).unwrap();
                wait_release.recv_timeout(Duration::from_secs(8)).unwrap();
                first = false;
            }
        });
        host
    });
    receive.recv_timeout(Duration::from_secs(8)).unwrap();
    assert!(!worker.is_finished());
    drop(retained); // The kernel can now release the original description.
    release.send(()).unwrap();
    let host = worker.join().unwrap();
    held(&host.store.lock().unwrap().db); // No test probe commits SQL drain.
    host.recover();
    host.unchanged();
    assert_eq!(attempt(&host.store.lock().unwrap().db).0, "submitted");
}

#[test]
fn post_spawn_registration_refusal_keeps_the_actual_child_and_private_pin() {
    if !isolated("post_spawn_registration_refusal_keeps_the_actual_child_and_private_pin") {
        return;
    }
    let mut host = Host::new(false);
    // The child really exits zero with a valid reply; registration did not bind.
    // Return its actual Child to run_owned, never Err/StartFailed after fork.
    assert!(host.run("normal", false, true).unwrap().is_ok());
    assert!(!host.registered.load(Ordering::Acquire));
    assert!(host.record().helper.is_none());
    assert_eq!(attempt(&host.store.lock().unwrap().db).0, "unknown");
    assert!(host.pin.lock().unwrap().is_some());
    host.held();
    host.unchanged();
    // Terminal private-fixture pin. No claim to reclaim unknown registration.
}

#[test]
fn file_reopen_cannot_finish_unknown_or_free_a_still_owned_pipe() {
    if !isolated("file_reopen_cannot_finish_unknown_or_free_a_still_owned_pipe") {
        return;
    }
    let mut host = Host::new(false);
    assert!(host.run("stdout", false, false).unwrap().is_err());
    let descendant = host.descendant();
    let reopened = Store::open(&host.location, &uuid::Uuid::new_v4().to_string()).unwrap();
    *host.store.lock().unwrap() = reopened;
    host.held();
    host.unchanged();
    let loaded = Loaded::load(&host.store.lock().unwrap().db, &host.admitted).unwrap();
    assert!(loaded
        .finish_submitted(
            &mut host.store.lock().unwrap().db,
            &host.ticket,
            &host.fixture.reply()
        )
        .is_err());
    host.release(&descendant);
    host.recover();
    host.unchanged();
    assert_eq!(attempt(&host.store.lock().unwrap().db).0, "unknown");
}

#[test]
fn actual_host_exit_and_reopen_preserve_the_surviving_synthetic_custody() {
    if let Some(root) = std::env::var_os("OWNED_PORT_CRASH_ROOT") {
        let mut host = Host::at(Path::new(&root), false);
        assert!(host.run("crash", false, false).unwrap().is_err());
        host.held();
        host.unchanged();
        // Exit only this exact synthetic fixture process. Its bounded survivor
        // retains inherited custody and exits at the parent's explicit gate.
        std::process::exit(0);
    }
    if !isolated("actual_host_exit_and_reopen_preserve_the_surviving_synthetic_custody") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let temp_path = temp.path().canonicalize().unwrap();
    let root = temp_path.join("crash");
    let mut child = Command::new(std::env::current_exe().unwrap());
    environment(&mut child, &temp_path.join("host-env"));
    let result = child
        .args([
            &format!(
                "{PREFIX}actual_host_exit_and_reopen_preserve_the_surviving_synthetic_custody"
            ),
            "--exact",
            "--nocapture",
        ])
        .env("OWNED_PORT_CRASH_ROOT", &root)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(result.status.success(), "private crash host failed");
    let fixture = Fixture::new();
    let location = Location {
        root: root.join("state/desktop"),
        configuration: fixture.binding.key.configuration.clone(),
    };
    let mut store = Store::open(&location, &uuid::Uuid::new_v4().to_string()).unwrap();
    let (attempt_row, record) = ledger::custody::pending(&store.db).unwrap().unwrap();
    let record = record.unwrap();
    assert_eq!(attempt_row.state, "unknown");
    held(&store.db);
    assert!(matches!(
        custody::probe(&location, &record),
        custody::Observation::Held
    ));
    let pid = fs::read_to_string(root.join("descendant"))
        .unwrap()
        .parse()
        .unwrap();
    let birth = platform::process_identity(pid).unwrap().birth;
    fs::write(
        root.join("release"),
        b"release surviving exact fixture descendant",
    )
    .unwrap();
    wait(|| {
        matches!(
            platform::process_state(pid, Some(&birth)),
            Ok(platform::ProcessState::Exited)
        )
    });
    assert_eq!(
        fs::read_to_string(root.join("released")).unwrap(),
        "explicit descendant gate"
    );
    wait(|| {
        store.retry_operation_custody();
        store.recover_operation_custody(&location);
        ledger::totals(&store.db).unwrap().undrained == 0
    });
    assert_eq!(attempt(&store.db).0, "unknown");
    let frozen = ledger::load(&store.db, &attempt_row.ticket.operation_id).unwrap();
    assert_eq!(frozen.raw_record, fixture.raw);
    assert_eq!(frozen.retained_input, fixture.input);
}
