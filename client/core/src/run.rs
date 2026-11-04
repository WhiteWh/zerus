//! Runs one [`Invocation`] and reports how it ended as an [`Exit`].
#[cfg(any(windows, test))]
mod hidden;
pub(crate) mod startup;
#[cfg(windows)]
pub use hidden::{HiddenError, ProcessIdentity};

use crate::actions::{Exit, Invocation};
use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod custody;

/// A noisy child keeps no more than this much of its standard error.
const STDERR_CAP: usize = 64 * 1024;
/// How long to wait for the readers once the child has exited.
const DRAIN: Duration = Duration::from_secs(2);
/// How long a child may keep running after a failed stdin write.
const WRITE_GRACE: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(20);

/// Runs `hgs` with `inv`, blocking for at most `inv.timeout` plus two seconds.
pub fn run(hgs: &Path, inv: &Invocation) -> Exit {
    run_with_env(hgs, inv, &[])
}

/// [`run`] with extra environment variables for the child. The agent's
/// attribution (`HGS_SESSION`, `HGS_RUN_ID`) is never passed on: the client
/// acts for the person even when it was started from an agent's shell.
///
/// A failed stdin write is a failure to deliver only while the child keeps
/// running; a child that exits without reading its input (a usage or unknown
/// session error) reports its real exit, as the Qt client does.
pub fn run_with_env(hgs: &Path, inv: &Invocation, parent_env: &[(&str, &str)]) -> Exit {
    match run_limited(hgs, inv, parent_env, usize::MAX) {
        Ok(exit) => exit,
        // No output passes usize::MAX bytes.
        Err(TooLarge { stderr, .. }) => Exit::Abnormal { stderr },
    }
}

/// Standard output passed the cap of a [`run_limited`] call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TooLarge {
    pub cap: usize,
    /// The standard error read by then.
    pub stderr: String,
}

/// [`run_with_env`] for reads: standard output of more than `stdout_cap`
/// bytes is an error. It keeps at most `stdout_cap + 1` bytes and ends the
/// child as soon as more arrive, so a peer cannot fill the client's memory
/// within the timeout. Output of exactly `stdout_cap` bytes is whole. With
/// `usize::MAX` nothing ends early, as for [`run_with_env`].
pub fn run_limited(
    hgs: &Path,
    inv: &Invocation,
    parent_env: &[(&str, &str)],
    stdout_cap: usize,
) -> Result<Exit, TooLarge> {
    limited(hgs, inv, parent_env, stdout_cap, Pipes::default())
}

/// Console-free Windows helper with bounded termination cleanup. A pending
/// original child remains retained and prevents another helper in this process;
/// an error never proves physical exit or authorizes replay. Only the supplied
/// environment is passed to this fixed helper; the parent's profile is omitted.
#[cfg(windows)]
pub fn run_hidden_limited(
    program: &Path,
    inv: &Invocation,
    parent_env: &[(&str, &str)],
    stdout_cap: usize,
) -> Result<Exit, HiddenError> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
    let stop = hidden::State::admit()?;
    let result = limited(
        program,
        inv,
        parent_env,
        stdout_cap,
        Pipes {
            spawn: Some(Box::new(|command| {
                command.creation_flags(CREATE_NO_WINDOW).spawn()
            })),
            hidden_stop: Some(stop.clone()),
            clear_environment: true,
            ..Pipes::default()
        },
    );
    stop.finish(result)
}

fn limited(
    program: &Path,
    inv: &Invocation,
    parent_env: &[(&str, &str)],
    stdout_cap: usize,
    pipes: Pipes,
) -> Result<Exit, TooLarge> {
    match run_inner(program, inv, parent_env, &[], stdout_cap, None, pipes) {
        Ok(exit) => Ok(exit),
        Err(Stopped::TooLarge(too_large)) => Err(too_large),
        // Nothing sets a flag when there is none.
        Err(Stopped::Cancelled) => Ok(Exit::Abnormal {
            stderr: "cancelled".into(),
        }),
    }
}

/// Why a [`run_cancellable`] call gave no exit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stopped {
    TooLarge(TooLarge),
    /// The cancel flag was set: the child was killed and waited for.
    Cancelled,
}

/// [`run_limited`] that ends the child, within one poll (20 ms), once
/// `cancel` is set, and reports [`Stopped::Cancelled`].
pub fn run_cancellable(
    hgs: &Path,
    inv: &Invocation,
    parent_env: &[(&str, &str)],
    stdout_cap: usize,
    cancel: &AtomicBool,
) -> Result<Exit, Stopped> {
    run_inner(
        hgs,
        inv,
        parent_env,
        &[],
        stdout_cap,
        Some(cancel),
        Pipes::default(),
    )
}

/// Opt-in lifetime accounting around the existing cancellable runner. Exit,
/// input-error, output and deadline semantics remain those of run_cancellable;
/// completion of this call does not claim real pipe completion. Copies of the
/// supplied charge live with the child, input writer and both output readers.
/// A retained pipe keeps its copy after this bounded call returns. Unverified
/// child exit pins a copy until host exit; the host must cap admissions.
/// No journal, execution authorization or effect retry is provided here.
pub fn run_accounted(
    hgs: &Path,
    inv: &Invocation,
    parent_env: &[(&str, &str)],
    stdout_cap: usize,
    cancel: &AtomicBool,
    resources: Arc<dyn Send + Sync>,
) -> Result<Exit, Stopped> {
    if cancel.load(Ordering::Acquire) {
        return Err(Stopped::Cancelled);
    }
    let child_exited = Arc::new(AtomicBool::new(false));
    let _child = ChildCharge {
        resources: Some(resources.clone()),
        exited: child_exited.clone(),
    };
    let result = run_inner(
        hgs,
        inv,
        parent_env,
        &[],
        stdout_cap,
        Some(cancel),
        Pipes {
            readers: Some(resources),
            retain_all: true,
            child_exited: Some(child_exited.clone()),
            ..Pipes::default()
        },
    );
    if matches!(result, Ok(Exit::StartFailed(_))) {
        child_exited.store(true, Ordering::Release);
    }
    result
}

/// [`run_limited`] of another program (a connector) that also leaves out
/// the variables in `remove`, such as the terminal's, even when
/// `parent_env` sets them.
pub fn run_without(
    program: &Path,
    inv: &Invocation,
    parent_env: &[(&str, &str)],
    remove: &[&str],
    stdout_cap: usize,
) -> Result<Exit, TooLarge> {
    match run_inner(
        program,
        inv,
        parent_env,
        remove,
        stdout_cap,
        None,
        Pipes::default(),
    ) {
        Ok(exit) => Ok(exit),
        Err(Stopped::TooLarge(too_large)) => Err(too_large),
        Err(Stopped::Cancelled) => Ok(Exit::Abnormal {
            stderr: "cancelled".into(),
        }),
    }
}

type StreamInput = Box<dyn FnOnce(ChildStdin) -> bool + Send>;
type StreamOutput = Box<dyn FnOnce(ChildStdout) -> bool + Send>;
type Spawn = fn(&mut Command) -> std::io::Result<Child>;
type GuardedSpawn = Box<dyn FnOnce(&mut Command) -> std::io::Result<Child> + Send>;

#[derive(Default)]
struct Pipes {
    input: Option<StreamInput>,
    output: Option<StreamOutput>,
    spawn: Option<GuardedSpawn>,
    readers: Option<Arc<dyn Send + Sync>>,
    stop: Option<Arc<AtomicBool>>,
    require_complete: bool,
    retain_all: bool,
    stderr_cap: Option<usize>,
    child_exited: Option<Arc<AtomicBool>>,
    clear_environment: bool,
    #[cfg(any(windows, test))]
    hidden_stop: Option<Arc<hidden::State>>,
}

struct StopContext {
    exited: Option<Arc<AtomicBool>>,
    #[cfg(any(windows, test))]
    hidden: Option<Arc<hidden::State>>,
}

pub(crate) struct ChildCharge {
    resources: Option<Arc<dyn Send + Sync>>,
    exited: Arc<AtomicBool>,
}
impl ChildCharge {
    pub(crate) fn new(resources: Arc<dyn Send + Sync>) -> Self {
        Self {
            resources: Some(resources),
            exited: Arc::new(AtomicBool::new(false)),
        }
    }
    pub(crate) fn confirmed(&self) {
        self.exited.store(true, Ordering::Release);
    }

    fn unverified(&mut self) -> Option<Arc<dyn Send + Sync>> {
        if self.exited.load(Ordering::Acquire) {
            None
        } else {
            self.resources.take()
        }
    }
}
impl Drop for ChildCharge {
    fn drop(&mut self) {
        if let Some(resources) = self.unverified() {
            // kill/wait errors do not prove child exit. Keep this admission
            // pinned rather than reporting a false last-resource completion.
            // The host must bound admissions and recover from verified durable
            // process/boot identities before allowing another operation.
            std::mem::forget(resources);
        }
    }
}

/// Body-free failures of an owned exchange. The caller retains its original
/// operation identity; none of these errors authorizes delivery or a retry.
#[derive(Debug, PartialEq, Eq)]
pub enum OwnedError {
    InvalidInput,
    Cancelled,
    TimedOut,
    StartFailed,
    WriteFailed,
    ChildFailed,
    OutputTooLarge,
    Incomplete,
    ExitUnverified,
}

/// One bounded exchange through the existing runner, with actual lifetime
/// accounting for the owned child, input writer and both output readers.
/// This supplies no authorization, executable selection or operation policy.
/// The host must reserve first and fence the supplied spawn hook itself.
///
/// Input/stdout are capped at 1 MiB, stderr at 16 KiB and the child deadline
/// at 30 seconds. Success requires exit zero, successful complete input and
/// real EOF on both pipes. A bounded stop/incomplete exchange may return while
/// a descendant retains a pipe; the shared resources guard then stays alive.
/// Only the owned child is stopped. No descendant or process group is killed.
/// An unverified wait failure conservatively pins one guard until this host
/// exits. The caller must cap admissions and supply durable recovery/remediation.
pub fn run_owned(
    hgs: &Path,
    inv: &Invocation,
    parent_env: &[(&str, &str)],
    cancel: &AtomicBool,
    spawn: impl FnOnce(&mut Command) -> std::io::Result<Child> + Send + 'static,
    resources: Arc<dyn Send + Sync>,
) -> Result<Vec<u8>, OwnedError> {
    run_owned_observed(hgs, inv, parent_env, cancel, spawn, resources, |_, _| {})
}

/// The same owned exchange, with a failure-only diagnostic observer. Standard
/// error is bounded to 16 KiB and remains untrusted: callers must select fixed
/// codes rather than persist child text. Observation changes no result, custody
/// or permission and never makes an uncertain exchange safe to replay.
pub fn run_owned_observed(
    hgs: &Path,
    inv: &Invocation,
    parent_env: &[(&str, &str)],
    cancel: &AtomicBool,
    spawn: impl FnOnce(&mut Command) -> std::io::Result<Child> + Send + 'static,
    resources: Arc<dyn Send + Sync>,
    observe: impl FnOnce(&OwnedError, Option<&str>),
) -> Result<Vec<u8>, OwnedError> {
    if cancel.load(Ordering::Acquire) {
        observe(&OwnedError::Cancelled, None);
        return Err(OwnedError::Cancelled);
    }
    if inv.stdin.as_ref().is_none_or(|bytes| bytes.len() > 1 << 20)
        || inv.timeout.is_zero()
        || inv.timeout > Duration::from_secs(30)
    {
        observe(&OwnedError::InvalidInput, None);
        return Err(OwnedError::InvalidInput);
    }
    let child_exited = Arc::new(AtomicBool::new(false));
    // Each pipe also owns a copy, even after the bounded runner returns.
    let _child = ChildCharge {
        resources: Some(resources.clone()),
        exited: child_exited.clone(),
    };
    let until = Instant::now() + inv.timeout;
    let exit = run_inner(
        hgs,
        inv,
        parent_env,
        &[],
        1 << 20,
        Some(cancel),
        Pipes {
            spawn: Some(Box::new(spawn)),
            readers: Some(resources),
            require_complete: true,
            stderr_cap: Some(16 << 10),
            child_exited: Some(child_exited.clone()),
            ..Pipes::default()
        },
    );
    if matches!(exit, Ok(Exit::StartFailed(_))) {
        // The guarded spawn hook returned no owned child.
        child_exited.store(true, Ordering::Release);
    }
    if !child_exited.load(Ordering::Acquire) {
        observe(&OwnedError::ExitUnverified, None);
        return Err(OwnedError::ExitUnverified);
    }
    // A cancellation/deadline racing completion cannot grant success.
    if cancel.load(Ordering::Acquire) {
        observe(&OwnedError::Cancelled, None);
        return Err(OwnedError::Cancelled);
    }
    if Instant::now() >= until {
        observe(&OwnedError::TimedOut, None);
        return Err(OwnedError::TimedOut);
    }
    match exit {
        Ok(Exit::Code {
            code: 0, stdout, ..
        }) => Ok(stdout),
        other => {
            let (error, stderr) = match other {
                Ok(Exit::Code { stderr, .. }) => (OwnedError::ChildFailed, Some(stderr)),
                Ok(Exit::TimedOut) => (OwnedError::TimedOut, None),
                Ok(Exit::StartFailed(_)) => (OwnedError::StartFailed, None),
                Ok(Exit::WriteFailed) => (OwnedError::WriteFailed, None),
                Ok(Exit::Abnormal { stderr }) => (OwnedError::Incomplete, Some(stderr)),
                Err(Stopped::Cancelled) => (OwnedError::Cancelled, None),
                Err(Stopped::TooLarge(_)) => (OwnedError::OutputTooLarge, None),
            };
            observe(&error, stderr.as_deref());
            Err(error)
        }
    }
}

// Also cancels a decoder that is still blocked in an OS read when the runner
// returns. Its reader guard remains held until that read actually ends.
struct CancelOutput(Option<Arc<AtomicBool>>);
impl Drop for CancelOutput {
    fn drop(&mut self) {
        if let Some(stop) = &self.0 {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

/// A streamed decoder never grants completion on a partial/failed child.
/// Process and pipe errors are static categories, without child output.
#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError<E> {
    Decode(E),
    Cancelled,
    TimedOut,
    StartFailed,
    ChildFailed,
    TooLarge,
    Incomplete,
}

/// Decode stdout on the existing runner's reader thread instead of collecting
/// it. The callback must bound input, memory and sink work, require real EOF,
/// and poll its stop flag/deadline. Its output/pages remain provisional until
/// this function succeeds: both pipes must end and the owned child must exit 0.
/// Stderr is capped at 64 KiB. `inv.stdin` must be absent.
///
/// The host supplies its guarded spawn function and an admitted reader-group
/// guard shared by both pipes. Admission must bound global jobs and exclude a
/// second reader group for the same source while an old group remains alive.
/// A descendant retaining a pipe keeps the guard after cancellation/deadline;
/// only the owned child is killed. No callback or unrelated process is killed.
/// Unverified owned-child exit also retains the supplied group until host exit;
/// a returned error and EOF alone cannot release that admission.
pub fn run_decoded<T: Send + 'static, E: Send + 'static>(
    hgs: &Path,
    inv: &Invocation,
    parent_env: &[(&str, &str)],
    cancel: &AtomicBool,
    spawn: Spawn,
    readers: Arc<dyn Send + Sync>,
    decode: impl FnOnce(ChildStdout, &AtomicBool, Instant) -> Result<T, E> + Send + 'static,
) -> Result<T, DecodeError<E>> {
    assert!(inv.stdin.is_none(), "a decoded read cannot have stdin");
    // Refuse before spawn if the caller has already cancelled.
    if cancel.load(Ordering::Relaxed) {
        return Err(DecodeError::Cancelled);
    }
    let child = ChildCharge::new(readers.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let decoder_stop = stop.clone();
    let deadline = Instant::now() + inv.timeout;
    let (tx, rx) = mpsc::sync_channel(1);
    let pipes = Pipes {
        output: Some(Box::new(move |pipe| {
            let result = decode(pipe, &decoder_stop, deadline);
            let ok = result.is_ok();
            tx.send(result).is_ok() && ok
        })),
        spawn: Some(Box::new(spawn)),
        readers: Some(readers),
        child_exited: Some(child.exited.clone()),
        stop: Some(stop),
        ..Pipes::default()
    };
    let exit = run_inner(hgs, inv, parent_env, &[], STDERR_CAP, Some(cancel), pipes);
    if matches!(exit, Ok(Exit::StartFailed(_))) {
        child.confirmed();
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(DecodeError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(DecodeError::TimedOut);
    }
    match exit {
        Err(Stopped::Cancelled) => return Err(DecodeError::Cancelled),
        Err(Stopped::TooLarge(_)) => return Err(DecodeError::TooLarge),
        Ok(Exit::TimedOut) => return Err(DecodeError::TimedOut),
        Ok(Exit::StartFailed(_)) => return Err(DecodeError::StartFailed),
        _ => {}
    }
    match rx.try_recv() {
        Ok(Err(error)) => Err(DecodeError::Decode(error)),
        Ok(Ok(value)) if matches!(exit, Ok(Exit::Code { code: 0, .. })) => Ok(value),
        Ok(Ok(_)) => Err(DecodeError::ChildFailed),
        Err(_) => Err(DecodeError::Incomplete),
    }
}

/// Streams bounded input on the existing runner's writer thread. Both output
/// pipes have a hard byte limit; exceeding either ends only this owned child.
/// The callback must bound its own work and returns false on encoding/write
/// failure. `inv.stdin` must be absent. A successful process exit alone does
/// not establish that the callback completed; the caller validates its result.
pub(crate) fn run_streamed(
    hgs: &Path,
    inv: &Invocation,
    parent_env: &[(&str, &str)],
    output_cap: usize,
    input: impl FnOnce(ChildStdin) -> bool + Send + 'static,
) -> Result<Exit, TooLarge> {
    streamed(hgs, inv, parent_env, output_cap, None, None, input)
}

/// The same bounded streaming exchange, retaining the caller's original source
/// guards through the child, writer and both actual pipe readers. A timeout or
/// cancellation does not imply those resources finished. No group is stopped.
pub(crate) fn run_streamed_retained(
    hgs: &Path,
    inv: &Invocation,
    parent_env: &[(&str, &str)],
    output_cap: usize,
    cancel: &AtomicBool,
    resources: Arc<dyn Send + Sync>,
    input: impl FnOnce(ChildStdin) -> bool + Send + 'static,
) -> Result<Exit, TooLarge> {
    streamed(
        hgs,
        inv,
        parent_env,
        output_cap,
        Some(cancel),
        Some(resources),
        input,
    )
}

fn streamed(
    hgs: &Path,
    inv: &Invocation,
    parent_env: &[(&str, &str)],
    output_cap: usize,
    cancel: Option<&AtomicBool>,
    resources: Option<Arc<dyn Send + Sync>>,
    input: impl FnOnce(ChildStdin) -> bool + Send + 'static,
) -> Result<Exit, TooLarge> {
    assert!(
        inv.stdin.is_none(),
        "streamed input cannot have a second body"
    );
    if cancel.is_some_and(|cancel| cancel.load(Ordering::Acquire)) {
        return Ok(Exit::TimedOut);
    }
    let child_exited = resources.as_ref().map(|_| Arc::new(AtomicBool::new(false)));
    let _child = resources.as_ref().map(|resources| ChildCharge {
        resources: Some(resources.clone()),
        exited: child_exited.as_ref().unwrap().clone(),
    });
    let result = run_inner(
        hgs,
        inv,
        parent_env,
        &[],
        output_cap,
        cancel,
        Pipes {
            input: Some(Box::new(input)),
            require_complete: resources.is_some(),
            readers: resources,
            child_exited: child_exited.clone(),
            ..Pipes::default()
        },
    );
    if matches!(result, Ok(Exit::StartFailed(_))) {
        if let Some(child_exited) = &child_exited {
            child_exited.store(true, Ordering::Release);
        }
    }
    match result {
        Ok(exit) => Ok(exit),
        Err(Stopped::TooLarge(error)) => Err(error),
        Err(Stopped::Cancelled) => Ok(Exit::TimedOut),
    }
}

fn run_inner(
    hgs: &Path,
    inv: &Invocation,
    parent_env: &[(&str, &str)],
    remove: &[&str],
    stdout_cap: usize,
    cancel: Option<&AtomicBool>,
    pipes: Pipes,
) -> Result<Exit, Stopped> {
    let stop_context = StopContext {
        exited: pipes.child_exited.clone(),
        #[cfg(any(windows, test))]
        hidden: pipes.hidden_stop.clone(),
    };
    let consumed = pipes.output.is_some();
    let retain_all = pipes.require_complete || pipes.retain_all;
    let strict_outputs = pipes.input.is_some() || consumed || pipes.require_complete;
    let stderr_cap = pipes.stderr_cap.unwrap_or(if strict_outputs {
        stdout_cap
    } else {
        STDERR_CAP
    });
    let _cancel_output = CancelOutput(pipes.stop);
    let mut command = Command::new(hgs);
    command.args(&inv.args);
    if pipes.clear_environment {
        command.env_clear();
    }
    for (key, value) in parent_env {
        command.env(key, value);
    }
    for key in remove {
        command.env_remove(key);
    }
    if !pipes.clear_environment {
        crate::child_environment::restore(&mut command);
    }
    command
        .env_remove("HGS_SESSION")
        .env_remove("HGS_RUN_ID")
        .stdin(
            if consumed && pipes.input.is_none() && inv.stdin.is_none() {
                Stdio::null()
            } else {
                Stdio::piped()
            },
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match pipes.spawn.unwrap_or_else(|| Box::new(Command::spawn))(&mut command) {
        Ok(child) => child,
        Err(error) => return Ok(Exit::StartFailed(error.to_string())),
    };
    if pipes.require_complete
        && (child.stdin.is_none() || child.stdout.is_none() || child.stderr.is_none())
    {
        return Ok(stop(
            child,
            Exit::Abnormal {
                stderr: "owned exchange requires all three pipes".into(),
            },
            &stop_context,
        ));
    }
    let deadline = Instant::now() + inv.timeout;

    let stdin = child.stdin.take();
    let input = inv.stdin.clone();
    let (write_tx, write_rx) = mpsc::channel();
    let input_guard = if retain_all {
        pipes.readers.clone()
    } else {
        None
    };
    thread::spawn(move || {
        let _input = input_guard;
        let ok = match (stdin, pipes.input, input) {
            (Some(pipe), Some(write), _) => write(pipe),
            (Some(mut pipe), None, Some(bytes)) => {
                pipe.write_all(&bytes).and_then(|_| pipe.flush()).is_ok()
            }
            _ => true,
        };
        let _ = write_tx.send(ok);
    });
    let (done_tx, done_rx) = mpsc::channel();
    let over = Arc::new(AtomicBool::new(false));
    let limit = (stdout_cap < usize::MAX).then(|| (stdout_cap, Arc::clone(&over)));
    let decoded = Arc::new(AtomicU8::new(0));
    let out = if let Some(consume) = pipes.output {
        let stdout = child.stdout.take();
        let done = done_tx.clone();
        let completed = decoded.clone();
        let guard = pipes.readers.clone();
        thread::spawn(move || {
            let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                stdout.is_some_and(consume)
            }))
            .unwrap_or(false);
            // Release the pipe group before waking a possible replacement.
            drop(guard);
            completed.store(if ok { 1 } else { 2 }, Ordering::Release);
            let _ = done.send(ok);
        });
        Arc::default()
    } else {
        reader(
            child.stdout.take(),
            if strict_outputs {
                stdout_cap
            } else {
                stdout_cap.saturating_add(1)
            },
            done_tx.clone(),
            limit,
            if retain_all {
                pipes.readers.clone()
            } else {
                None
            },
        )
    };
    let err = reader(
        child.stderr.take(),
        stderr_cap,
        done_tx,
        strict_outputs.then(|| (stderr_cap, Arc::clone(&over))),
        pipes.readers,
    );
    let too_large = |err: &Shared| {
        Stopped::TooLarge(TooLarge {
            cap: stdout_cap,
            stderr: String::from_utf8_lossy(&take(err)).into_owned(),
        })
    };

    let mut written = false;
    // Set when a write failed: the child gets a short grace to show it exited.
    let mut write_failed_until = None;
    let status = loop {
        if !written {
            match write_rx.try_recv() {
                Ok(true) => written = true,
                Err(TryRecvError::Disconnected) if !pipes.require_complete => written = true,
                Ok(false) | Err(TryRecvError::Disconnected) => {
                    written = true;
                    write_failed_until = Some((Instant::now() + WRITE_GRACE).min(deadline));
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            let _ = stop(child, Exit::TimedOut, &stop_context);
            drain(&done_rx);
            return Err(Stopped::Cancelled);
        }
        if over.load(Ordering::Relaxed) {
            // Ended for its output: the exit that gives is not reported.
            let _ = stop(child, Exit::TimedOut, &stop_context);
            drain(&done_rx);
            return Err(too_large(&err));
        }
        if consumed && decoded.load(Ordering::Acquire) == 2 {
            let exit = stop(
                child,
                Exit::Abnormal {
                    stderr: "stdout decoder failed".into(),
                },
                &stop_context,
            );
            drain(&done_rx);
            return Ok(exit);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if let Some(exited) = &pipes.child_exited {
                    exited.store(true, Ordering::Release);
                }
                break status;
            }
            Ok(None) => {}
            Err(error) => {
                return Ok(stop(
                    child,
                    Exit::Abnormal {
                        stderr: error.to_string(),
                    },
                    &stop_context,
                ))
            }
        }
        let now = Instant::now();
        if write_failed_until.is_some_and(|until| now >= until) {
            return Ok(stop(child, Exit::WriteFailed, &stop_context));
        }
        if now >= deadline {
            let exit = stop(child, Exit::TimedOut, &stop_context);
            if consumed {
                drain(&done_rx);
            }
            return Ok(exit);
        }
        thread::sleep(POLL);
    };
    if consumed || pipes.require_complete {
        let until = deadline.min(Instant::now() + DRAIN);
        let mut ended = 0;
        while ended < 2 || (pipes.require_complete && !written) {
            if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                return Err(Stopped::Cancelled);
            }
            if over.load(Ordering::Relaxed) {
                return Err(too_large(&err));
            }
            if pipes.require_complete && !written {
                match write_rx.try_recv() {
                    Ok(true) => written = true,
                    Ok(false) | Err(TryRecvError::Disconnected) => return Ok(Exit::WriteFailed),
                    Err(TryRecvError::Empty) => (),
                }
            }
            if pipes.require_complete && write_failed_until.is_some() {
                return Ok(Exit::WriteFailed);
            }
            if ended == 2 && written {
                break;
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(if Instant::now() >= deadline {
                    Exit::TimedOut
                } else {
                    Exit::Abnormal {
                        stderr: "stdout decoder or pipe incomplete".into(),
                    }
                });
            }
            if ended == 2 {
                // Both readers ended; only the owned input writer is pending.
                // Its counterpart may be a descendant. Do not spin on the now
                // disconnected reader-completion channel.
                thread::sleep(left.min(POLL));
                continue;
            }
            match done_rx.recv_timeout(left.min(POLL)) {
                Ok(false) if pipes.require_complete => {
                    return Ok(Exit::Abnormal {
                        stderr: "owned pipe did not reach EOF".into(),
                    })
                }
                Ok(_) => ended += 1,
                Err(mpsc::RecvTimeoutError::Disconnected) if pipes.require_complete => {
                    return Ok(Exit::Abnormal {
                        stderr: "owned reader did not complete".into(),
                    });
                }
                Err(_) => (),
            }
        }
        if consumed && decoded.load(Ordering::Acquire) != 1 {
            return Ok(Exit::Abnormal {
                stderr: "stdout decoder failed".into(),
            });
        }
    } else {
        drain(&done_rx);
    }
    let stdout = take(&out);
    // A child can exit before the loop sees its output pass the cap.
    if stdout.len() > stdout_cap || over.load(Ordering::Relaxed) {
        return Err(too_large(&err));
    }
    let stderr = String::from_utf8_lossy(&take(&err)).into_owned();
    Ok(match status.code() {
        Some(code) => Exit::Code {
            code,
            stdout,
            stderr,
        },
        None => Exit::Abnormal { stderr },
    })
}

/// Waits for both pipe readers to finish. One budget for both pipes: a
/// grandchild that keeps them open must not hold the call, and what was read
/// by then is still returned.
fn drain(done: &Receiver<bool>) {
    let until = Instant::now() + DRAIN;
    for _ in 0..2 {
        let left = until.saturating_duration_since(Instant::now());
        if done.recv_timeout(left).is_err() {
            break;
        }
    }
}

fn stop(mut child: Child, exit: Exit, context: &StopContext) -> Exit {
    #[cfg(any(windows, test))]
    if let Some(state) = &context.hidden {
        return state.stop(child, exit, context.exited.clone());
    }
    let _ = child.kill();
    if child.wait().is_ok() {
        if let Some(exited) = &context.exited {
            exited.store(true, Ordering::Release);
        }
    }
    exit
}

type Shared = Arc<Mutex<Vec<u8>>>;

fn take(buffer: &Shared) -> Vec<u8> {
    std::mem::take(&mut *buffer.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Reads a pipe to its end on its own thread into a shared buffer, keeping at
/// most `cap` bytes and draining the rest so the child never blocks on a full
/// pipe. The buffer holds what was read so far even if the pipe never closes;
/// `done` gets a signal at the end. With `limit`, the flag is set once more
/// than its byte count has been read.
fn reader(
    pipe: Option<impl Read + Send + 'static>,
    cap: usize,
    done: Sender<bool>,
    limit: Option<(usize, Arc<AtomicBool>)>,
    guard: Option<Arc<dyn Send + Sync>>,
) -> Shared {
    let buffer: Shared = Arc::default();
    let shared = Arc::clone(&buffer);
    thread::spawn(move || {
        let mut eof = false;
        if let Some(mut pipe) = pipe {
            let mut chunk = [0u8; 8192];
            let mut received = 0usize;
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(n) => {
                        received = received.saturating_add(n);
                        let mut kept = shared.lock().unwrap_or_else(|e| e.into_inner());
                        let room = cap.saturating_sub(kept.len()).min(n);
                        kept.extend_from_slice(&chunk[..room]);
                        if let Some((bytes, over)) = &limit {
                            if received > *bytes {
                                over.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        }
        drop(guard);
        let _ = done.send(eof);
    });
    buffer
}

#[cfg(test)]
mod owned_charge_tests {
    use super::*;

    #[test]
    fn an_unverified_child_keeps_its_resource_charge() {
        let resource = Arc::new(());
        let weak = Arc::downgrade(&resource);
        let mut charge = ChildCharge {
            resources: Some(resource),
            exited: Arc::new(AtomicBool::new(false)),
        };
        // Exercise the exact unverified branch without leaking a test guard.
        // Drop retains this guard indefinitely; only external verified recovery
        // may settle the host's durable charge after this process is gone.
        let pinned = charge.unverified().unwrap();
        drop(charge);
        assert!(weak.upgrade().is_some());
        drop(pinned);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn verified_child_exit_releases_only_its_own_guard_copy() {
        let resource = Arc::new(());
        let weak = Arc::downgrade(&resource);
        let charge = ChildCharge {
            resources: Some(resource.clone()),
            exited: Arc::new(AtomicBool::new(true)),
        };
        drop(charge);
        assert!(weak.upgrade().is_some());
        drop(resource);
        assert!(weak.upgrade().is_none());
    }
}

#[cfg(all(test, unix))]
mod stream_tests {
    use super::*;

    fn python(code: &str, timeout: Duration) -> Invocation {
        Invocation {
            args: vec!["-c".into(), code.into()],
            stdin: None,
            timeout,
            request_id: None,
        }
    }

    #[test]
    fn an_explicit_helper_environment_omits_every_parent_variable() {
        let call = Invocation {
            args: vec![],
            stdin: None,
            timeout: Duration::from_secs(5),
            request_id: None,
        };
        let result = run_inner(
            Path::new("/usr/bin/env"),
            &call,
            &[("ZERUS_TEST_ONLY", "explicit")],
            &[],
            1024,
            None,
            Pipes {
                clear_environment: true,
                ..Pipes::default()
            },
        )
        .unwrap();
        assert!(
            matches!(result, Exit::Code { code: 0, stdout, .. } if stdout == b"ZERUS_TEST_ONLY=explicit\n")
        );
    }

    #[test]
    fn streamed_eof_and_both_exact_output_limits_are_preserved() {
        let code = "import sys,os; assert sys.stdin.buffer.read() == b'bounded input'; assert 'HGS_SESSION' not in os.environ; sys.stdout.write('x'*16384); sys.stderr.write('y'*16384)";
        let exit = run_streamed(
            Path::new("python3"),
            &python(code, Duration::from_secs(5)),
            &[("HGS_SESSION", "discard")],
            16384,
            |mut pipe| pipe.write_all(b"bounded input").is_ok(),
        )
        .unwrap();
        match exit {
            Exit::Code {
                code: 0,
                stdout,
                stderr,
            } => {
                assert_eq!(stdout.len(), 16384);
                assert_eq!(stderr.len(), 16384);
            }
            other => panic!("{other:?}"),
        }
        for noisy in ["stdout", "stderr"] {
            let began = Instant::now();
            let code = format!("import sys,time; sys.{noisy}.write('x'*16385); sys.{noisy}.flush(); time.sleep(10)");
            let error = run_streamed(
                Path::new("python3"),
                &python(&code, Duration::from_secs(5)),
                &[],
                16384,
                |_| true,
            )
            .unwrap_err();
            assert_eq!(error.cap, 16384);
            assert!(error.stderr.len() <= 16384);
            assert!(began.elapsed() < Duration::from_secs(3));
        }
    }

    #[test]
    fn deadline_ends_owned_child_and_releases_its_blocked_pipe_writer() {
        let (done, completed) = mpsc::channel();
        let began = Instant::now();
        let exit = run_streamed(
            Path::new("python3"),
            &python("import time; time.sleep(10)", Duration::from_millis(250)),
            &[],
            16384,
            move |mut pipe| {
                let chunk = [0; 8192];
                let result = (0..16384).try_for_each(|_| pipe.write_all(&chunk));
                let _ = done.send(result.is_err());
                result.is_ok()
            },
        )
        .unwrap();
        assert_eq!(exit, Exit::TimedOut);
        assert!(began.elapsed() < Duration::from_secs(3));
        assert!(completed.recv_timeout(Duration::from_secs(2)).unwrap());
    }

    #[test]
    fn retained_streaming_keeps_the_original_guard_until_the_writer_really_ends() {
        let resource = Arc::new(());
        let weak = Arc::downgrade(&resource);
        let (release, wait) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        let exit = run_streamed_retained(
            Path::new("python3"),
            &python("pass", Duration::from_millis(250)),
            &[],
            1024,
            &AtomicBool::new(false),
            resource,
            move |_original_pipe| {
                let result = wait.recv_timeout(Duration::from_secs(5)).is_ok();
                let _ = done.send(());
                result
            },
        )
        .unwrap();
        assert!(!matches!(exit, Exit::Code { code: 0, .. }));
        assert!(
            weak.upgrade().is_some(),
            "bounded return is not writer completion"
        );
        release.send(()).unwrap();
        completed.recv_timeout(Duration::from_secs(2)).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while weak.upgrade().is_some() && Instant::now() < until {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn cancelled_retained_streaming_admits_no_child_or_writer() {
        let resource = Arc::new(());
        let weak = Arc::downgrade(&resource);
        let exit = run_streamed_retained(
            Path::new("a-helper-that-must-not-start"),
            &python("pass", Duration::from_secs(1)),
            &[],
            1024,
            &AtomicBool::new(true),
            resource,
            |_| panic!("cancelled source started a writer"),
        )
        .unwrap();
        assert_eq!(exit, Exit::TimedOut);
        assert!(weak.upgrade().is_none());
    }
}

#[cfg(all(test, windows))]
mod hidden_tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine};

    const CASE: &str = "run::hidden_tests::hidden_helper_child";
    const MARKER: &str = "ZERUS_TEST_HIDDEN_HELPER_CASE";

    #[test]
    #[ignore = "exact-filter hidden-helper child, launched only by its parent test"]
    fn hidden_helper_child() {
        // Fail before I/O or spawning if the marker was lost. This child has
        // a different name from its parent and can never re-enter that test.
        assert_eq!(
            std::env::var(MARKER).as_deref(),
            Ok(CASE),
            "hidden-helper child requires its explicit marker"
        );
        use windows_sys::Win32::System::Console::GetConsoleWindow;
        // No PowerShell startup or C# compilation runs inside this deadline.
        // SAFETY: GetConsoleWindow has no preconditions.
        assert!(unsafe { GetConsoleWindow() }.is_null());
        let mut input = Vec::new();
        std::io::stdin().take(1025).read_to_end(&mut input).unwrap();
        assert_eq!(input, b"exact helper input");
        println!("\n0:exact helper input");
    }

    #[test]
    fn a_hidden_helper_has_no_console_and_receives_exact_input_through_eof() {
        const FIXTURE_WAIT: Duration = Duration::from_secs(15);
        let system_root = std::env::var("SystemRoot").expect("Windows has Unicode SystemRoot");
        let program = std::path::PathBuf::from(&system_root)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let modules = program
            .parent()
            .unwrap()
            .join("Modules")
            .to_str()
            .unwrap()
            .to_owned();
        let temp = std::env::temp_dir().to_str().unwrap().to_owned();
        let environment = [
            ("SystemRoot", system_root.as_str()),
            ("WINDIR", system_root.as_str()),
            ("PSModulePath", modules.as_str()),
            ("TEMP", temp.as_str()),
            ("TMP", temp.as_str()),
        ];
        let mut native_environment = environment.to_vec();
        native_environment.push((MARKER, CASE));
        let call = Invocation {
            args: vec![
                "--exact".into(),
                CASE.into(),
                "--ignored".into(),
                "--nocapture".into(),
                "--test-threads=1".into(),
            ],
            stdin: Some(b"exact helper input".to_vec()),
            timeout: FIXTURE_WAIT,
            request_id: None,
        };
        let result = run_hidden_limited(
            &std::env::current_exe().unwrap(),
            &call,
            &native_environment,
            1024,
        )
        .unwrap();
        match result {
            Exit::Code {
                code: 0, stdout, ..
            } => {
                let output = String::from_utf8(stdout).unwrap();
                assert!(output.lines().any(|line| line == "0:exact helper input"));
                assert!(output.contains("test result: ok. 1 passed; 0 failed; 0 ignored;"));
            }
            other => panic!("{other:?}"),
        }

        // Exercise the original env-loss hazard through the same hidden
        // launch path. The child must fail its guard, never invoke a parent.
        let missing_marker =
            run_hidden_limited(&std::env::current_exe().unwrap(), &call, &environment, 4096)
                .unwrap();
        match missing_marker {
            Exit::Code {
                code: 101,
                stdout,
                stderr,
            } => {
                let output = String::from_utf8(stdout).unwrap();
                assert!(output.contains("test result: FAILED. 0 passed; 1 failed;"));
                assert!(format!("{output}{stderr}")
                    .contains("hidden-helper child requires its explicit marker"));
            }
            other => panic!("missing marker: {other:?}"),
        }

        let body = "[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);[Console]::WriteLine($env:PSModulePath);[Console]::WriteLine($env:ZERUS_TEST_PARENT_PROFILE);[Console]::WriteLine([string]$PSModuleAutoLoadingPreference)";
        let probe = |source: &str| Invocation {
            args: vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-EncodedCommand".into(),
                STANDARD.encode(
                    source
                        .encode_utf16()
                        .flat_map(u16::to_le_bytes)
                        .collect::<Vec<_>>(),
                ),
            ],
            stdin: Some(Vec::new()),
            timeout: FIXTURE_WAIT,
            request_id: None,
        };
        let inherited = run_limited(
            &program,
            &probe(&format!("$PSModuleAutoLoadingPreference='All';{body}")),
            &[
                ("PSModulePath", r"C:\Example\UserModules"),
                ("ZERUS_TEST_PARENT_PROFILE", "inherited"),
            ],
            4096,
        )
        .unwrap();
        let minimal = run_hidden_limited(&program, &probe(&format!(
            "$PSModuleAutoLoadingPreference='None';$env:PSModulePath=[IO.Path]::Combine($PSHOME,'Modules');{body}"
        )), &environment, 4096).unwrap();
        let Exit::Code {
            code: 0,
            stdout: before,
            ..
        } = inherited
        else {
            panic!("inherited probe: {inherited:?}")
        };
        let Exit::Code {
            code: 0,
            stdout: after,
            ..
        } = minimal
        else {
            panic!("minimal probe: {minimal:?}")
        };
        let before = String::from_utf8(before).unwrap();
        let after = String::from_utf8(after).unwrap();
        assert!(before.contains(r"C:\Example\UserModules"));
        assert!(before.contains("inherited"));
        assert!(before.trim_end().ends_with("All"));
        let after: Vec<_> = after.lines().collect();
        assert_eq!(after.len(), 3);
        // PathBuf retains literal '/' separators, while $PSHOME prints '\\'.
        // Compare these known fixture paths with Windows path spelling rules.
        assert_eq!(
            after[0].replace('/', "\\").to_lowercase(),
            modules.replace('/', "\\").to_lowercase()
        );
        assert_eq!(&after[1..], ["", "None"]);
    }
}
