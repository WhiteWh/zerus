//! Fake holder/actor replies, REAL child/pipe/resource lifetime. These controls
//! establish no Root authentication, native confirmation, signal or removal.
use super::*;
use crate::desktop_service::{
    termination_outcome::tests::{capture, connected, fixture},
    tests::eventually,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};
use zerus_client_core::{
    worktree_programs::{stop, DirectoryId},
    Action,
};

const OWNER: &str = r#"#!/usr/bin/python3
import json,os,pathlib,sys,time,uuid
root=pathlib.Path(__file__).parent
args=sys.argv[1:]
assert args.pop(0)=='--desktop-helper-rendezvous-v1'
assert sys.stdin.buffer.readline()==b'HGS-ORIGINAL-CHILD-V1\n'
if args==['--help']:
    print('Worktree program custody ABI: worktree-program-custody-v1',flush=True);sys.exit(0)
assert args[:1]==['--expect-machine'] and args[2:]==['worktrees','program-holder','--json']
first=json.loads(sys.stdin.buffer.readline())
manifest=json.loads((root/'program-manifest').read_text())
print(json.dumps(dict(op='ready',abi='worktree-program-custody-v1',pipeline=first['pipeline'],manifest=manifest,
    holder=dict(pid=os.getpid(),parent=os.getppid(),birth='fixture-data',audit=None),socket='/tmp/example/pins')),flush=True)
# A gated holder's detached descendant keeps its output open until released,
# even after the holder replies Closed or is killed.
if (root/'program-holder-gate').exists() and os.fork()==0:
    os.setsid()
    until=time.monotonic()+20
    while not (root/'program-holder-release').exists() and time.monotonic()<until:time.sleep(.005)
    os._exit(0)
while True:
    frame=json.loads(sys.stdin.buffer.readline());op=frame['op']
    with (root/'program-controls').open('a') as out:out.write(op+'\n')
    if op=='arm':
        # Return the precomputed Rust binding; wire integrity is not under test.
        binding=json.loads((root/'program-binding').read_text())
        print(json.dumps(dict(op='armed',binding=binding)),flush=True)
    elif op=='joined':print(json.dumps(dict(op='retired',binding=frame['binding'])),flush=True)
    elif op=='observe':
        current=dict(manifest);current['id']=str(uuid.uuid4())
        print(json.dumps(dict(op='observed',pipeline=first['pipeline'],observation=frame['observation'],manifest=current)),flush=True)
    elif op=='close':
        print(json.dumps(dict(op='closed',pipeline=first['pipeline'])),flush=True);break
    else:raise AssertionError(op)
"#;
const MUTATOR: &str = r#"#!/usr/bin/python3
import os,pathlib,time
root=pathlib.Path(__file__).parent
if os.fork()==0:
    if (root/'program-close-stdio').exists():
        os.setsid()
        for fd in (0,1,2):
            null=os.open('/dev/null',os.O_RDWR);os.dup2(null,fd)
            if null>2:os.close(null)
    (root/'program-reader-started').write_text('started')
    until=time.monotonic()+20
    while not (root/'program-release').exists() and time.monotonic()<until:time.sleep(.005)
    (root/'program-reader-ended').write_text('ended')
    os._exit(0)
print('parsed result, not a join',flush=True)
"#;
struct Release(PathBuf);
impl Drop for Release {
    fn drop(&mut self) {
        let _ = fs::write(self.0.join("program-release"), b"release");
        let until = Instant::now() + Duration::from_secs(5);
        while self.0.join("program-reader-started").exists()
            && !self.0.join("program-reader-ended").exists()
            && Instant::now() < until
        {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
#[cfg(target_os = "linux")]
thread_local! {
    static SCRIPT_WRITERS: std::cell::RefCell<Option<Vec<ForkedWriter>>> = const { std::cell::RefCell::new(None) };
}

// A sibling fork can inherit even a CLOEXEC writer until it execs. Hold that
// real descriptor reference to make the scheduling window deterministic. The
// forked child executes only async-signal-safe close/read/_exit, never an effect.
#[cfg(target_os = "linux")]
struct ForkedWriter(libc::pid_t, libc::c_int);
#[cfg(target_os = "linux")]
impl ForkedWriter {
    fn new() -> Self {
        let mut pipe = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                libc::close(pipe[0]);
                libc::close(pipe[1]);
            }
            panic!("fixture sibling fork: {error}");
        }
        if pid == 0 {
            unsafe {
                libc::close(pipe[1]);
                let mut byte = 0u8;
                while libc::read(pipe[0], (&mut byte as *mut u8).cast(), 1) < 0 {
                    if *libc::__errno_location() != libc::EINTR {
                        break;
                    }
                }
                libc::_exit(0);
            }
        }
        unsafe { libc::close(pipe[0]) };
        Self(pid, pipe[1])
    }
}
#[cfg(target_os = "linux")]
impl Drop for ForkedWriter {
    fn drop(&mut self) {
        unsafe { libc::close(self.1) };
        let mut status = 0;
        loop {
            let waited = unsafe { libc::waitpid(self.0, &mut status, 0) };
            if waited == self.0 {
                assert_eq!(status, 0);
                break;
            }
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EINTR)
            );
        }
    }
}
#[cfg(target_os = "linux")]
struct ScriptWriters;
#[cfg(target_os = "linux")]
impl ScriptWriters {
    fn arm() -> Self {
        SCRIPT_WRITERS.with_borrow_mut(|writers| {
            assert!(writers.replace(Vec::new()).is_none());
        });
        Self
    }
}
#[cfg(target_os = "linux")]
impl Drop for ScriptWriters {
    fn drop(&mut self) {
        SCRIPT_WRITERS.with_borrow_mut(|writers| drop(writers.take()));
    }
}
fn executable(path: &Path, text: &str) {
    use std::io::Write;
    let mut file = fs::File::create(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
    #[cfg(target_os = "linux")]
    if path.file_name().is_some_and(|name| name == "holder") {
        SCRIPT_WRITERS.with_borrow_mut(|writers| {
            if let Some(writers) = writers {
                use std::os::fd::AsRawFd;
                let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
                assert!(flags >= 0);
                assert_ne!(flags & libc::FD_CLOEXEC, 0);
                writers.push(ForkedWriter::new());
            }
        });
    }
    drop(file);
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
// Publish once before CacheServer/Service can fork. A CLOEXEC writer inherited
// by an unrelated pre-exec child still makes Linux refuse exec with ETXTBSY.
fn program_fixture() -> crate::desktop_service::tests::Fixture {
    let f = fixture(json!({}));
    executable(&f.0.join("holder"), OWNER);
    executable(&f.0.join("mutator"), MUTATOR);
    f
}

fn manifest() -> Manifest {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut capture = capture();
    capture.owner_id = uuid::Uuid::new_v4().to_string();
    Manifest {
        abi: zerus_client_core::worktree_programs::ABI.into(),
        id: uuid::Uuid::new_v4().to_string(),
        capture,
        configuration: "a".repeat(64),
        boot: uuid::Uuid::new_v4().to_string(),
        checkout: DirectoryId {
            device: 1,
            inode: 2,
            mount: "fixture-object".into(),
        },
        issued: now,
        expires: now + 300,
        programs: Vec::new(),
    }
}

#[test]
fn a_parsed_result_and_holder_ack_cannot_retire_real_readers_or_a_mutator_loan() {
    if crate::test_context::isolated_desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for closed_stdio in [false, true] {
            let f = program_fixture();
            let (service, _cache) = connected(&f).await;
            let actions = crate::actions::Actions::new(f.0.join("marks"));
            let key = crate::actions::Lane::key(&capture().target, &Action::Terminate);
            let hold = Arc::new(actions.begin(key.clone(), &Action::Terminate).unwrap());
            let context = Context::new_supported(service.clone(), hold.clone()).unwrap();
            let mut manifest = manifest();
            manifest.programs = [
                (10, "agent"),
                (20, "hgs"),
                (30, "hgs"),
                (40, "agent descendant"),
            ]
            .into_iter()
            .map(
                |(pid, label)| zerus_client_core::worktree_programs::Program {
                    pid,
                    birth: format!("native-{pid}"),
                    label: label.into(),
                },
            )
            .collect();
            let display = zerus_client_core::worktree_programs::presentation::Presentation::new(
                &manifest,
                zerus_client_core::worktree_programs::presentation::Identity {
                    pid: 10,
                    birth: "native-10".into(),
                },
                zerus_client_core::worktree_programs::presentation::Identity {
                    pid: 20,
                    birth: "native-20".into(),
                },
            );
            let pipeline = uuid::Uuid::new_v4().to_string();
            let input = stop::Input {
                request_id: uuid::Uuid::new_v4().to_string(),
                confirmation_id: uuid::Uuid::new_v4().to_string(),
                source: stop::Source::Service {
                    epoch: context.scope.service_epoch.clone(),
                },
                original: serde_json::to_string(
                    &json!({"request_id":uuid::Uuid::new_v4().to_string(),
                "action":"terminate","expected_run_id":manifest.capture.target.run_id,
                "expected_conversation_id":manifest.capture.target.conversation_id}),
                )
                .unwrap(),
                manifest: manifest.clone(),
                archive_id: uuid::Uuid::new_v4().to_string(),
                stop: stop::Kind::Normal,
            };
            let binding = custody::Binding::for_input(&pipeline, &input);
            fs::write(
                f.0.join("program-manifest"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            fs::write(f.0.join("program-binding"), binding.bytes().unwrap()).unwrap();
            let mut owner = Owner::begin(
                &f.0.join("holder"),
                Control::Open {
                    presentation: false,
                    pipeline: pipeline.clone(),
                    discovery: Discovery {
                        capture: manifest.capture.clone(),
                    },
                },
                manifest.configuration.clone(),
                context.environment.clone(),
                context.clone(),
                Arc::new(Command::spawn),
            )
            .unwrap();
            let not_after = owner.deadline();
            let Reply::Ready { holder, socket, .. } = owner.ready().unwrap() else {
                panic!("Ready")
            };
            owner
                .arm(
                    &input,
                    NativeChild {
                        pid: 99999,
                        parent: std::process::id(),
                        birth: "fixture-data".into(),
                        audit: None,
                    },
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
            let source = Arc::new(Source {
                presentation: Some(display),
                context: Arc::new(Origin::Service(context)),
                owner: Mutex::new(owner),
                manifest,
                pipeline,
                holder,
                socket,
                not_after,
                normal_consumed: AtomicBool::new(false),
                closed: AtomicBool::new(false),
            });
            let (mut mutation, resources) = Mutation::new(source.clone(), &input);
            if closed_stdio {
                fs::write(f.0.join("program-close-stdio"), b"closed").unwrap();
            }
            let inherited = resources.descendants.clone();
            let mut loan = Some(resources.clone());
            let weak = Arc::downgrade(&resources);
            let _release = Release(f.0.clone());
            let inv = Invocation {
                args: Vec::new(),
                stdin: None,
                timeout: Duration::from_secs(5),
                request_id: None,
            };
            // The actual runner, not a Boolean/read receipt, owns these references.
            let result = run::run_accounted_frozen_with_completion(
                &f.0.join("mutator"),
                &inv,
                source.context.environment(),
                1024,
                &AtomicBool::new(false),
                (resources, Some(mutation.completion.clone())),
                move |command| {
                    inherited.inherit(command)?;
                    command.spawn()
                },
            )
            .unwrap();
            assert!(matches!(result, Exit::Code { code: 0, .. }));
            assert!(!mutation.try_retire().unwrap());
            assert_eq!(
                fs::read_to_string(f.0.join("program-controls")).unwrap(),
                "arm\n"
            );
            if closed_stdio {
                eventually(
                    "the original child and all stdio readers have actually ended",
                    || weak.strong_count() == 1,
                )
                .await;
                drop(loan.take());
                assert!(
                    !mutation.try_retire().unwrap(),
                    "the detached original descendant still holds the phase despite closed stdio"
                );
                assert_eq!(
                    fs::read_to_string(f.0.join("program-controls")).unwrap(),
                    "arm\n"
                );
            }
            fs::write(f.0.join("program-release"), b"release").unwrap();
            if !closed_stdio {
                eventually(
                    "actual mutator readers joined, only the original loan remains",
                    || weak.strong_count() == 1,
                )
                .await;
                assert!(
                    !mutation.try_retire().unwrap(),
                    "a mutator-owned loan also prevents Force admission"
                );
            }
            drop(loan.take());
            {
                let observed = std::cell::RefCell::new(&mut mutation);
                eventually(
                    "the independent original descendant custody has physically settled",
                    || observed.borrow_mut().try_retire().unwrap(),
                )
                .await;
            }
            assert!(
                mutation.try_retire().unwrap(),
                "retirement is idempotent, never a new phase"
            );
            assert_eq!(
                fs::read_to_string(f.0.join("program-controls")).unwrap(),
                "arm\njoined\n"
            );
            assert!(
                actions.begin(key.clone(), &Action::Terminate).is_none(),
                "the identified read-only source remains charged"
            );
            let initial_completion = Arc::new(run::Completion::new());
            let initial = run::run_accounted_frozen_with_completion(
                &f.0.join("mutator"),
                &inv,
                source.context.environment(),
                1024,
                &AtomicBool::new(false),
                (source.clone(), Some(initial_completion.clone())),
                move |command| command.spawn(),
            )
            .unwrap();
            assert!(matches!(initial, Exit::Code { code: 0, .. }));
            eventually("initial native call I/O joined", || {
                initial_completion.join() == run::Retirement::Retired
            })
            .await;
            // zerus-1y897.5/.7: unarmed phases, each with its own real child,
            // pipes and loan; they stay unsettled until the control retires them.
            let mut loan: Option<Arc<dyn Send + Sync>> = None;
            let mut lock: Option<Arc<dyn Send + Sync>> = None;
            let unarmed = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13].map(|index| {
                let (unarmed, resources) = Mutation::new(source.clone(), &input);
                unarmed.armed.store(false, Ordering::Release);
                if index == 7 {
                    // zerus-1y897.8: extra references to this phase's loan and to
                    // its inherited descendant lock keep it undrained.
                    loan = Some(resources.clone());
                    lock = Some(resources.descendants.clone());
                }
                let inherited = resources.descendants.clone();
                let exit = run::run_accounted_frozen_with_completion(
                    &f.0.join("mutator"),
                    &inv,
                    source.context.environment(),
                    1024,
                    &AtomicBool::new(false),
                    (resources, Some(unarmed.completion.clone())),
                    move |command| {
                        inherited.inherit(command)?;
                        command.spawn()
                    },
                )
                .unwrap();
                assert!(matches!(exit, Exit::Code { code: 0, .. }));
                unarmed
            });
            let never_ran = [(); 3].map(|()| Mutation::new(source.clone(), &input).0);
            super::super::pipeline::assert_presentation_views(
                source.clone(),
                hold,
                input,
                initial_completion,
                mutation,
                unarmed,
                (loan.unwrap(), lock.unwrap(), never_ran),
            );
            assert!(
                !source.current(),
                "closed original custody cannot become current again"
            );
            drop(source);
            assert!(actions.begin(key, &Action::Terminate).is_some());
            service.join().await.unwrap();
        }
    });
}

/// zerus-1y897.6: a real source whose holder output stays open (a detached
/// descendant holds it) until `program-holder-release` exists, with a settled
/// refused-stop phase and a joined original call. `not_after` shortens the
/// source's own deadline.
fn gated_refusal(
    root: &Path,
    context: Arc<Context>,
    not_after: Option<Instant>,
) -> (Arc<Source>, stop::Input, Arc<run::Completion>, Mutation) {
    let manifest = manifest();
    let pipeline = uuid::Uuid::new_v4().to_string();
    let input = stop::Input {
        request_id: uuid::Uuid::new_v4().to_string(),
        confirmation_id: uuid::Uuid::new_v4().to_string(),
        source: stop::Source::Service {
            epoch: context.scope.service_epoch.clone(),
        },
        original: serde_json::to_string(&json!({"request_id": uuid::Uuid::new_v4().to_string(),
            "action": "terminate", "expected_run_id": manifest.capture.target.run_id,
            "expected_conversation_id": manifest.capture.target.conversation_id}))
        .unwrap(),
        manifest: manifest.clone(),
        archive_id: uuid::Uuid::new_v4().to_string(),
        stop: stop::Kind::Normal,
    };
    let binding = custody::Binding::for_input(&pipeline, &input);
    fs::write(
        root.join("program-manifest"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    fs::write(root.join("program-binding"), binding.bytes().unwrap()).unwrap();
    fs::write(root.join("program-holder-gate"), b"gate").unwrap();
    let _ = fs::remove_file(root.join("program-holder-release"));
    // The mutator's forked reader ends at once; only the holder is gated.
    fs::write(root.join("program-release"), b"release").unwrap();
    let mut owner = Owner::begin(
        &root.join("holder"),
        Control::Open {
            presentation: false,
            pipeline: pipeline.clone(),
            discovery: Discovery {
                capture: manifest.capture.clone(),
            },
        },
        manifest.configuration.clone(),
        context.environment.clone(),
        context.clone(),
        Arc::new(|command| {
            let child = command.spawn();
            if let Err(error) = &child {
                eprintln!("fixture holder spawn failed: {error}");
            }
            child
        }),
    )
    .unwrap();
    let not_after = not_after.unwrap_or(owner.deadline());
    let Reply::Ready { holder, socket, .. } = owner.ready().unwrap() else {
        panic!("Ready")
    };
    owner
        .arm(
            &input,
            NativeChild {
                pid: 99999,
                parent: std::process::id(),
                birth: "fixture-data".into(),
                audit: None,
            },
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
    let source = Arc::new(Source {
        presentation: None,
        context: Arc::new(Origin::Service(context)),
        owner: Mutex::new(owner),
        manifest,
        pipeline,
        holder,
        socket,
        not_after,
        normal_consumed: AtomicBool::new(false),
        closed: AtomicBool::new(false),
    });
    let inv = Invocation {
        args: Vec::new(),
        stdin: None,
        timeout: Duration::from_secs(5),
        request_id: None,
    };
    let (mut mutation, resources) = Mutation::new(source.clone(), &input);
    let inherited = resources.descendants.clone();
    let stop_run = run::run_accounted_frozen_with_completion(
        &root.join("mutator"),
        &inv,
        source.context.environment(),
        1024,
        &AtomicBool::new(false),
        (resources, Some(mutation.completion.clone())),
        move |command| {
            inherited.inherit(command)?;
            command.spawn()
        },
    )
    .unwrap();
    assert!(matches!(stop_run, Exit::Code { code: 0, .. }));
    let until = Instant::now() + Duration::from_secs(10);
    while !mutation.try_retire().unwrap() {
        assert!(Instant::now() < until, "the stop phase settled");
        std::thread::sleep(Duration::from_millis(10));
    }
    let completion = Arc::new(run::Completion::new());
    let original = run::run_accounted_frozen_with_completion(
        &root.join("mutator"),
        &inv,
        source.context.environment(),
        1024,
        &AtomicBool::new(false),
        (source.clone(), Some(completion.clone())),
        move |command| command.spawn(),
    )
    .unwrap();
    assert!(matches!(original, Exit::Code { code: 0, .. }));
    while completion.join() != run::Retirement::Retired {
        assert!(Instant::now() < until, "the original call joined");
        std::thread::sleep(Duration::from_millis(10));
    }
    (source, input, completion, mutation)
}

/// zerus-1y897.6: a known refusal's Check again while its holder close is
/// delayed, through the production publish, passive watcher and Check.
#[test]
fn a_refused_check_waits_for_a_delayed_holder_close_and_reviews_once() {
    if crate::test_context::isolated_desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let f = program_fixture();
        let (service, _cache) = connected(&f).await;
        #[cfg(target_os = "linux")]
        let _writers = ScriptWriters::arm();
        let actions = crate::actions::Actions::new(f.0.join("marks"));
        let key = crate::actions::Lane::key(&capture().target, &Action::Terminate);
        let _release = Release(f.0.clone());
        for expired in [false, true] {
            let hold = Arc::new(
                actions
                    .begin(key.clone(), &Action::Terminate)
                    .expect("the previous source released its hold"),
            );
            let context = Context::new_supported(service.clone(), hold).unwrap();
            let not_after = expired.then(|| Instant::now() + Duration::from_secs(3));
            let (source, input, completion, mutation) = gated_refusal(&f.0, context, not_after);
            let root = f.0.clone();
            super::super::pipeline::assert_delayed_close(
                source,
                input,
                completion,
                mutation,
                expired,
                move || fs::write(root.join("program-holder-release"), b"release").unwrap(),
            );
        }
        assert!(
            actions.begin(key, &Action::Terminate).is_some(),
            "a held source outlived its control"
        );
        service.join().await.unwrap();
    });
}
