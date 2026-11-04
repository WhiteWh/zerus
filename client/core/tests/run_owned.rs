//! Actual owned synthetic children and short-lived descendants. No agents,
//! service registration, session state or process-group cleanup.
#![cfg(unix)]

use std::{
    io,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use zerus_client_core::{
    run::{run_owned, run_owned_observed, OwnedError},
    Invocation,
};

struct Guard(Arc<AtomicBool>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
fn guard() -> (Arc<dyn Send + Sync>, Arc<AtomicBool>) {
    let dropped = Arc::new(AtomicBool::new(false));
    (Arc::new(Guard(dropped.clone())), dropped)
}
fn wait_for_release(dropped: &AtomicBool) {
    // Inherited-pipe grandchildren sleep 4 s so a slow runner cannot release
    // them before the "still owns admission" check; allow for that here.
    let until = Instant::now() + Duration::from_secs(10);
    while !dropped.load(Ordering::Acquire) && Instant::now() < until {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        dropped.load(Ordering::Acquire),
        "owned resources did not end"
    );
}
fn spawn(command: &mut Command) -> io::Result<Child> {
    command.env("OWNED_SPAWN_HOOK", "yes").spawn()
}
fn invocation(code: &str, input: Vec<u8>, timeout: Duration) -> Invocation {
    Invocation {
        args: vec!["-c".into(), code.into()],
        stdin: Some(input),
        timeout,
        request_id: None,
    }
}
fn run(code: &str, input: Vec<u8>) -> Result<Vec<u8>, OwnedError> {
    let (resources, dropped) = guard();
    let result = run_owned(
        Path::new("python3"),
        &invocation(code, input, Duration::from_secs(5)),
        &[],
        &AtomicBool::new(false),
        spawn,
        resources,
    );
    wait_for_release(&dropped);
    result
}

#[test]
fn diagnostic_observation_preserves_failure_and_original_resource_accounting() {
    let (resources, dropped) = guard();
    let mut observed = None;
    let result = run_owned_observed(
        Path::new("python3"),
        &invocation("import sys;sys.stdin.buffer.read();sys.stderr.write('hgs: frontend process image unavailable\\n');sys.exit(1)", vec![], Duration::from_secs(5)),
        &[], &AtomicBool::new(false), spawn, resources,
        |error, stderr| observed = Some((format!("{error:?}"), stderr.unwrap_or("").to_owned())),
    );
    assert_eq!(result, Err(OwnedError::ChildFailed));
    assert_eq!(
        observed,
        Some((
            "ChildFailed".into(),
            "hgs: frontend process image unavailable\n".into()
        ))
    );
    wait_for_release(&dropped);
}

#[test]
fn exact_input_and_output_limits_require_full_input_eof_and_zero_exit() {
    let input = "exact UTF-8: пример\n".as_bytes().repeat(1024);
    let (resources, dropped) = guard();
    let output = run_owned(
        Path::new("python3"),
        &invocation(
            r#"import os,sys
assert os.environ['OWNED_SPAWN_HOOK'] == 'yes'
assert 'HGS_SESSION' not in os.environ and 'HGS_RUN_ID' not in os.environ
data=sys.stdin.buffer.read()
assert data == 'exact UTF-8: пример\n'.encode()*1024
sys.stderr.buffer.write(b'e'*16384)
sys.stdout.buffer.write(b'x'*1048576)
"#,
            input,
            Duration::from_secs(5),
        ),
        &[("HGS_SESSION", "discard"), ("HGS_RUN_ID", "discard")],
        &AtomicBool::new(false),
        spawn,
        resources,
    )
    .unwrap();
    assert_eq!(output, vec![b'x'; 1 << 20]);
    wait_for_release(&dropped);
}

#[test]
fn oversized_missing_input_invalid_deadline_and_pre_cancel_never_spawn() {
    let calls = Arc::new(AtomicUsize::new(0));
    for (input, timeout, cancel, error) in [
        (
            Some(vec![0; (1 << 20) + 1]),
            1,
            false,
            OwnedError::InvalidInput,
        ),
        (None, 1, false, OwnedError::InvalidInput),
        (Some(vec![]), 31, false, OwnedError::InvalidInput),
        (Some(vec![]), 0, false, OwnedError::InvalidInput),
        (Some(vec![]), 1, true, OwnedError::Cancelled),
    ] {
        let (resources, dropped) = guard();
        let mut inv = invocation(
            "raise Exception('never')",
            vec![],
            Duration::from_secs(timeout),
        );
        inv.stdin = input;
        let count = calls.clone();
        let result = run_owned(
            Path::new("python3"),
            &inv,
            &[],
            &AtomicBool::new(cancel),
            move |command| {
                count.fetch_add(1, Ordering::Relaxed);
                spawn(command)
            },
            resources,
        );
        assert_eq!(result, Err(error));
        assert!(dropped.load(Ordering::Acquire));
    }
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn guarded_spawn_refusal_exposes_no_private_error_and_releases_admission() {
    let (resources, dropped) = guard();
    let result = run_owned(
        Path::new("python3"),
        &invocation("", vec![], Duration::from_secs(1)),
        &[],
        &AtomicBool::new(false),
        |_| Err(io::Error::other("private host diagnostic")),
        resources,
    );
    assert_eq!(result, Err(OwnedError::StartFailed));
    assert!(!format!("{result:?}").contains("private"));
    assert!(dropped.load(Ordering::Acquire));
}

#[test]
fn a_guarded_spawn_cannot_succeed_without_every_owned_pipe() {
    for missing in 0..3 {
        let (resources, dropped) = guard();
        let result = run_owned(
            Path::new("python3"),
            &invocation("print('{}')", vec![], Duration::from_secs(5)),
            &[],
            &AtomicBool::new(false),
            move |command| {
                match missing {
                    0 => command.stdin(Stdio::null()),
                    1 => command.stdout(Stdio::null()),
                    _ => command.stderr(Stdio::null()),
                };
                spawn(command)
            },
            resources,
        );
        assert_eq!(
            result,
            Err(OwnedError::Incomplete),
            "missing pipe {missing}"
        );
        wait_for_release(&dropped);
    }
}

#[test]
fn nonzero_exit_cannot_return_an_otherwise_valid_reply() {
    assert_eq!(
        run(
            "import sys;sys.stdin.buffer.read();print('{}');sys.exit(7)",
            vec![]
        ),
        Err(OwnedError::ChildFailed)
    );
}

#[test]
fn incomplete_input_never_settles_an_early_successful_child_reply() {
    assert_eq!(
        run("import os;os.close(0);print('{}')", vec![b'x'; 1 << 20]),
        Err(OwnedError::WriteFailed)
    );
}

#[test]
fn stdout_and_stderr_have_independent_hard_limits() {
    for code in [
        "import sys,time;sys.stdin.buffer.read();sys.stdout.buffer.write(b'x'*1048577);sys.stdout.flush();time.sleep(10)",
        "import sys,time;sys.stdin.buffer.read();sys.stderr.buffer.write(b'e'*16385);sys.stderr.flush();time.sleep(10)",
    ] {
        let began = Instant::now();
        assert_eq!(run(code, vec![]), Err(OwnedError::OutputTooLarge));
        assert!(began.elapsed() < Duration::from_secs(3));
    }
}

#[test]
fn timeout_requires_owned_child_exit_even_when_both_outputs_have_closed() {
    let (resources, dropped) = guard();
    let began = Instant::now();
    let result = run_owned(
        Path::new("python3"),
        &invocation(
            "import os,sys,time;sys.stdin.buffer.read();print('{}',flush=True);os.close(1);os.close(2);time.sleep(10)",
            vec![],
            Duration::from_millis(200),
        ),
        &[],
        &AtomicBool::new(false),
        spawn,
        resources,
    );
    assert_eq!(result, Err(OwnedError::TimedOut));
    assert!(began.elapsed() < Duration::from_secs(1));
    wait_for_release(&dropped);
}

#[test]
fn cancellation_ends_the_owned_child_and_blocked_input_writer() {
    let cancel = Arc::new(AtomicBool::new(false));
    let set = cancel.clone();
    let setter = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        set.store(true, Ordering::Release);
    });
    let (resources, dropped) = guard();
    let began = Instant::now();
    let result = run_owned(
        Path::new("python3"),
        &invocation(
            "import time;time.sleep(10)",
            vec![b'x'; 1 << 20],
            Duration::from_secs(5),
        ),
        &[],
        &cancel,
        spawn,
        resources,
    );
    setter.join().unwrap();
    assert_eq!(result, Err(OwnedError::Cancelled));
    assert!(began.elapsed() < Duration::from_secs(3));
    wait_for_release(&dropped);
}

#[test]
fn inherited_stdout_keeps_admission_after_bounded_return_without_partial_success() {
    let (resources, dropped) = guard();
    let result = run_owned(
        Path::new("python3"),
        &invocation(
            "import subprocess,sys;sys.stdin.buffer.read();print('{}',flush=True);subprocess.Popen([sys.executable,'-c','import time;time.sleep(4)'],stdin=subprocess.DEVNULL,stderr=subprocess.DEVNULL)",
            vec![],
            Duration::from_millis(200),
        ),
        &[],
        &AtomicBool::new(false),
        spawn,
        resources,
    );
    assert_eq!(result, Err(OwnedError::TimedOut));
    assert!(
        !dropped.load(Ordering::Acquire),
        "stdout still owns admission"
    );
    wait_for_release(&dropped);
}

#[test]
fn inherited_stdin_keeps_admission_after_child_and_both_outputs_end() {
    let (resources, dropped) = guard();
    let result = run_owned(
        Path::new("python3"),
        &invocation(
            "import subprocess,sys;subprocess.Popen([sys.executable,'-c','import time;time.sleep(4)'],stdin=sys.stdin,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);print('{}',flush=True)",
            vec![b'x'; 1 << 20],
            Duration::from_millis(200),
        ),
        &[],
        &AtomicBool::new(false),
        spawn,
        resources,
    );
    assert_eq!(result, Err(OwnedError::TimedOut));
    assert!(
        !dropped.load(Ordering::Acquire),
        "stdin writer still owns admission"
    );
    wait_for_release(&dropped);
}
