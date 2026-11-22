//! The bounded runner of one original child of a desktop actor, shared by the
//! creation launch and task message leaves (zerus-5wmh.66.4.4.3). Each kind
//! keeps its own custody variable, input, retained output, reason codes and
//! result type in its own `child` module; the mechanics here are one.
//!
//! The child is this computer's own `hgs` with the plan's exact arguments, in
//! its own process group. It inherits the effect's close-only custody lock
//! and, before anything else, marks it close-on-exec ([`adopt`]): the lock
//! lives exactly as long as the child, and nothing it starts inherits it.
//! Its input is closed or exactly the kind's frozen bytes, written without
//! blocking; both outputs are drained without blocking, and one of them is
//! kept within the kind's bound. Only that child is ever signalled, and only
//! at the deadline or when its wait cannot be observed; its process group,
//! tmux and agents never are.
//!
//! A kind that names a resources guard keeps it beyond this call: an output
//! pipe still open on return keeps a guard copy in its own reader until its
//! real end, and an end that can never be observed (a failed read or an
//! unverified wait) pins a copy until this process exits. A kind without one
//! closes its pipes on return.
use crate::platform;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zerus_client_core::run::{
    custody::InheritedLock,
    original_input::{self, Owner as InputOwner},
};

/// Prepares `command` to execute the service's pinned image (zerus-5sl7r):
/// the pin's own inheritance, and the path that image was copied from as
/// `HGS_EXECUTABLE`, unless the service itself names one. The child runs a
/// sealed memfd, so its `/proc/self/exe` is `/memfd:… (deleted)`; without
/// this a resume put that name into its terminal's command, whose shell
/// could not run it, and the pane ended before its supervisor started. The
/// caller has just checked `image.current()`: that path still holds the
/// pinned bytes. A later replacement (an update) makes the terminal run the
/// newly installed `hgs`, as `hgs resume` run by hand would, and its
/// supervisor records which one it runs.
#[cfg(target_os = "linux")]
pub(in crate::desktop) fn inherit_pinned(
    image: &zerus_client_core::image::linux::Pin,
    command: &mut Command,
) -> std::io::Result<()> {
    image.inherit(command)?;
    if std::env::var_os("HGS_EXECUTABLE").is_none_or(|value| value.is_empty()) {
        command.env("HGS_EXECUTABLE", image.source());
    }
    Ok(())
}

/// Called first in `main` for each kind's variable `var`. A child started by
/// a desktop actor holds the actor's close-only custody lock for exactly its
/// own lifetime: the inherited descriptor is marked close-on-exec before
/// anything else runs, and the variable naming it is removed. A malformed
/// value refuses with `invalid`, an unusable descriptor with `unavailable`,
/// before the child starts anything.
pub(in crate::desktop) fn adopt(var: &str, invalid: &str, unavailable: &str) -> Result<(), String> {
    let Some(value) = std::env::var_os(var) else {
        return Ok(());
    };
    // Single-threaded: main has started nothing yet.
    std::env::remove_var(var);
    let fd = value
        .to_str()
        .filter(|text| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|text| text.parse::<i32>().ok())
        .filter(|fd| *fd > 2)
        .ok_or(invalid)?;
    // SAFETY: fcntl on a descriptor number; failure is reported, never ignored.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
            return Err(unavailable.into());
        }
    }
    Ok(())
}

/// Frozen stdin: the record's bytes, or an exact input file read only
/// through its kept handle.
// Only the task message leaf, fenced on macOS, constructs one there.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(in crate::desktop) enum Stdin<'a> {
    Bytes(&'a [u8]),
    File(&'a std::fs::File, u64),
}
impl Stdin<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes.len(),
            Self::File(_, bytes) => *bytes as usize,
        }
    }
    /// The bytes from `at` on that the next write offers; a file is read at
    /// that offset, never from a shared position.
    fn from<'b>(&'b self, at: usize, buffer: &'b mut [u8]) -> std::io::Result<&'b [u8]> {
        match self {
            Self::Bytes(bytes) => Ok(&bytes[at..]),
            Self::File(file, bytes) => {
                use std::os::unix::fs::FileExt;
                let left = (*bytes as usize).saturating_sub(at).min(buffer.len());
                let read = file.read_at(&mut buffer[..left], at as u64)?;
                if read == 0 && left > 0 {
                    return Err(std::io::ErrorKind::UnexpectedEof.into());
                }
                Ok(&buffer[..read])
            }
        }
    }
    /// All of it, for a test executor.
    #[cfg(test)]
    pub(in crate::desktop) fn to_vec(&self) -> std::io::Result<Vec<u8>> {
        let mut buffer = vec![0; self.len()];
        let mut at = 0;
        while at < self.len() {
            let mut chunk = vec![0; 64 << 10];
            let next = self.from(at, &mut chunk)?.to_vec();
            buffer[at..at + next.len()].copy_from_slice(&next);
            at += next.len();
        }
        Ok(buffer)
    }
}

/// The child's input.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(in crate::desktop) enum Input<'a> {
    /// Closed (`/dev/null`).
    Closed,
    /// Exactly `stdin`; an exit before all of it was written is
    /// `Unknown(unwritten)`.
    Frozen {
        stdin: Stdin<'a>,
        unwritten: &'static str,
    },
}

/// The one output kept; the other is drained and discarded.
#[cfg_attr(target_os = "macos", allow(dead_code))]
#[derive(Clone, Copy)]
pub(in crate::desktop) enum Output {
    /// The first `n` bytes of stderr; more is discarded.
    StderrPrefix(usize),
    /// All of stdout up to `bytes`; more makes the result
    /// `Unknown(too_large)`.
    Stdout {
        bytes: usize,
        too_large: &'static str,
    },
}

/// One kind's reasons for an Unknown result.
#[derive(Clone, Copy)]
pub(in crate::desktop) struct Codes {
    /// The wait could not be observed (the child was then killed).
    pub unobserved: &'static str,
    /// An output pipe could not be read; its end is never assumed.
    pub unreadable: &'static str,
    /// A signal ended the child.
    pub signalled: &'static str,
    /// The deadline passed while the child ran (the child alone was killed).
    pub timed_out: &'static str,
    /// The child exited, but another process kept its output open past the
    /// deadline.
    pub retained: &'static str,
}

/// The guard a kind's still-open pipes keep until their real end, and the
/// name of the reader that keeps it.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(in crate::desktop) struct Retain<'a> {
    pub guard: &'a Arc<dyn Send + Sync>,
    pub reader: &'static str,
}

/// One kind's child.
pub(in crate::desktop) struct Spec<'a> {
    /// Names the inherited custody descriptor.
    pub custody_env: &'static str,
    pub input: Input<'a>,
    pub output: Output,
    pub codes: Codes,
    /// `None` closes every original pipe on return.
    pub retain: Option<Retain<'a>>,
}

/// How the original child ended.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::desktop) enum Ended {
    /// It exited with `code` within the deadline, both output pipes ended and
    /// its whole input was written; `output` is the kept output.
    Exited { code: i32, output: Vec<u8> },
    /// It never started: refused before any child existed (the own image, its
    /// custody, the kind's final check, or the spawn itself, which reports a
    /// fork or exec failure before any program code runs). Nothing ran.
    NotStarted,
    /// Its result is unknown, for the kind's named reason.
    Unknown(&'static str),
}

#[cfg(test)]
type InputOwnerHook = Box<dyn FnOnce(&mut InputOwner)>;

#[cfg(test)]
thread_local! {
    /// Makes the next spawn on this thread fail inside `pre_exec`, as a custody
    /// refusal there would.
    pub(in crate::desktop) static REFUSE_PRE_EXEC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    // One-shot observation faults on the original executor thread. Anonymous
    // pipes do not safely provide deterministic EIO, and provoking wait errors
    // by stealing waitpid/SIGCHLD would interfere with other test children.
    pub(in crate::desktop) static READ_ERROR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(in crate::desktop) static WAIT_ERROR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    // The same wait fault, once the child's first kept output was read.
    pub(in crate::desktop) static WAIT_ERROR_AFTER_OUTPUT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    // Native outcomes observed by controls on this executor thread. In
    // particular Darwin can reject shutdown after the peer has been killed.
    static SPAWN_ERROR: std::cell::Cell<Option<(std::io::ErrorKind, Option<i32>)>> = const { std::cell::Cell::new(None) };
    static INPUT_FINISH_ERROR: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
    static INPUT_BLOCKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static INPUT_BLOCKED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    pub(in crate::desktop) static SPAWNED: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
    /// Runs once on this thread after the final check, right before spawn.
    pub(in crate::desktop) static AFTER_FINAL_CHECK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    /// Runs once on this thread right after the spawn, before any input.
    pub(in crate::desktop) static AFTER_SPAWN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    /// Runs once on this thread as part of an input read, after its bytes
    /// arrived: a slow source.
    pub(in crate::desktop) static INPUT_READ: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static INPUT_OWNER: std::cell::RefCell<Option<InputOwnerHook>> = const { std::cell::RefCell::new(None) };
    static INPUT_FINISH: std::cell::RefCell<Option<InputOwnerHook>> = const { std::cell::RefCell::new(None) };
    /// How many input bytes the last child on this thread was offered.
    pub(in crate::desktop) static OFFERED: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// An original output pipe.
trait Pipe: Read + AsRawFd + Send {}
impl<T: Read + AsRawFd + Send> Pipe for T {}

/// Keeps `guard` as long as this process lives: the end it waits for can
/// never be observed.
fn pin(guard: &Arc<dyn Send + Sync>) {
    std::mem::forget(guard.clone());
}

/// Keeps an original output pipe, and with it a copy of the guard, until the
/// pipe's real end. What it reads is discarded and nothing is signalled.
fn retain(mut pipe: Box<dyn Pipe>, retain: &Retain<'_>) {
    let fd = pipe.as_raw_fd();
    // SAFETY: fcntl on a descriptor this reader now owns; reads block from here.
    unsafe {
        libc::fcntl(
            fd,
            libc::F_SETFL,
            libc::fcntl(fd, libc::F_GETFL) & !libc::O_NONBLOCK,
        );
    }
    let held = retain.guard.clone();
    let reader = std::thread::Builder::new()
        .name(retain.reader.into())
        .spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => {
                        pin(&held);
                        return;
                    }
                }
            }
            // The real end: only now does this copy of the guard go.
            drop(held);
        });
    if reader.is_err() {
        pin(retain.guard);
    }
}

/// Runs `program args` as `spec`'s original child under `custody`, to its end
/// or to `deadline`. `final_check` runs inside the spawn turn, last before the
/// spawn: its refusal means that no child ever existed.
/// The final check may return a guard, held through spawn and dropped as soon
/// as spawn returns, including failure. It is never held while the child runs.
pub(in crate::desktop) fn run<T>(
    program: &Path,
    args: &[String],
    spec: Spec<'_>,
    custody: &Arc<InheritedLock>,
    deadline: Instant,
    final_check: impl FnOnce(&mut Command) -> std::io::Result<T>,
) -> Ended {
    start(program, args, spec, Some(custody), deadline, final_check)
}

/// Runs a child that holds no custody, by the same rules: a read without
/// any effect of its own (the deferred answer's observation), which no
/// drain waits for. It inherits no lock and names none in `spec`'s variable.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(in crate::desktop) fn run_unheld(
    program: &Path,
    args: &[String],
    spec: Spec<'_>,
    deadline: Instant,
    final_check: impl FnOnce(&mut Command) -> std::io::Result<()>,
) -> Ended {
    start(program, args, spec, None, deadline, final_check)
}

fn start<T>(
    program: &Path,
    args: &[String],
    spec: Spec<'_>,
    custody: Option<&Arc<InheritedLock>>,
    deadline: Instant,
    final_check: impl FnOnce(&mut Command) -> std::io::Result<T>,
) -> Ended {
    #[cfg(test)]
    {
        SPAWN_ERROR.with(|slot| slot.set(None));
        INPUT_FINISH_ERROR.with(|slot| slot.set(None));
        INPUT_BLOCKS.with(|slot| slot.set(0));
    }
    let Spec {
        custody_env,
        input,
        output,
        codes,
        retain: kept,
    } = spec;
    let pin_guard = || {
        if let Some(retain) = &kept {
            pin(retain.guard);
        }
    };
    let (input, unwritten) = match input {
        Input::Closed => (None, ""),
        Input::Frozen { stdin, unwritten } => (Some(stdin), unwritten),
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .process_group(0)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(custody) = custody {
        command.env(custody_env, custody.as_raw_fd().to_string());
    }
    let spawned = {
        let _turn = platform::spawn_turn();
        input
            .as_ref()
            .map(|input| original_input::prepare_in_turn(input.len() > 0, &_turn))
            .transpose()
            .and_then(|input| {
                let spawn = |command: &mut Command| {
                    custody
                        .map_or(Ok(()), |custody| custody.inherit(command))
                        .and_then(|()| final_check(command))
                        .and_then(|admission| {
                            #[cfg(test)]
                            if REFUSE_PRE_EXEC.with(|refuse| refuse.replace(false)) {
                                // SAFETY: returns an error only; nothing else runs after fork.
                                unsafe {
                                    command.pre_exec(|| {
                                        Err(std::io::Error::from_raw_os_error(libc::EPERM))
                                    });
                                }
                            }
                            #[cfg(test)]
                            if let Some(hook) =
                                AFTER_FINAL_CHECK.with(|hook| hook.borrow_mut().take())
                            {
                                hook();
                            }
                            // The never-renewed deadline, last before the spawn: a budget
                            // the checks or the wait for this turn spent starts nothing.
                            if Instant::now() >= deadline {
                                return Err(std::io::ErrorKind::TimedOut.into());
                            }
                            let spawned = command.spawn();
                            drop(admission);
                            spawned
                        })
                };
                match input {
                    Some(input) => input.spawn_with(&mut command, spawn),
                    None => spawn(&mut command),
                }
            })
    };
    // Only these pre-child branches mean "not started"; once a child exists,
    // every outcome is its exit or Unknown.
    #[cfg(test)]
    if let Err(error) = &spawned {
        SPAWN_ERROR.with(|slot| slot.set(Some((error.kind(), error.raw_os_error()))));
    }
    let Ok(mut child) = spawned else {
        return Ended::NotStarted;
    };
    #[cfg(test)]
    SPAWNED.with(|spawned| spawned.set(Some(child.id())));
    #[cfg(test)]
    if let Some(hook) = AFTER_SPAWN.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
    // The frozen stdin, written without blocking inside the same drain loop,
    // so neither side can block the other and no writer outlives this call.
    let mut chunk = vec![0u8; if input.is_some() { 64 << 10 } else { 0 }];
    let mut stdin = child.stdin.take();
    // Never duplicate a writer before the guarded spawn has accepted it.
    // Failure after a child exists is Unknown, never permission to replay.
    let mut input_failed = false;
    let mut input_owner = match stdin
        .as_ref()
        .map(|writer| InputOwner::acquire(writer, input.as_ref().is_some_and(|i| i.len() > 0)))
        .transpose()
    {
        Ok(owner) => owner,
        Err(_) => {
            input_failed = true;
            stdin = None;
            None
        }
    };
    #[cfg(test)]
    if let Some(owner) = &mut input_owner {
        if let Some(hook) = INPUT_OWNER.with(|hook| hook.borrow_mut().take()) {
            hook(owner);
        }
    }

    let mut written = 0;
    let stdout = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    for fd in [
        stdin.as_ref().map(AsRawFd::as_raw_fd),
        stdout.as_ref().map(AsRawFd::as_raw_fd),
        stderr_pipe.as_ref().map(AsRawFd::as_raw_fd),
    ]
    .into_iter()
    .flatten()
    {
        // The input is written and both outputs drained while the child runs.
        // SAFETY: fcntl on descriptors this function owns.
        unsafe {
            libc::fcntl(
                fd,
                libc::F_SETFL,
                libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK,
            );
        }
    }
    let mut pipes: [Option<Box<dyn Pipe>>; 2] = [
        stdout.map(|pipe| Box::new(pipe) as Box<dyn Pipe>),
        stderr_pipe.map(|pipe| Box::new(pipe) as Box<dyn Pipe>),
    ];
    let mut kept_output = Vec::new();
    let mut overflow = None;
    let mut status = None;
    // A pipe that cannot be read is never taken as its end.
    let mut unreadable = false;
    let ended = loop {
        if let (Some(pipe), Some(input)) = (stdin.as_mut(), input.as_ref()) {
            let mut finished = false;
            loop {
                if written == input.len() {
                    // All of it: finish the input so the child sees its end.
                    finished = true;
                    break;
                }
                let Ok(next) = input.from(written, &mut chunk) else {
                    // An input that cannot be read is never complete.
                    finished = true;
                    break;
                };
                #[cfg(test)]
                if let Some(hook) = INPUT_READ.with(|hook| hook.borrow_mut().take()) {
                    hook();
                }
                // The never-renewed deadline, after the read and right before
                // each write, its retries included: no byte is offered once it
                // passed, however it was spent. A child that exists is then
                // killed below, and its result stays Unknown.
                if Instant::now() >= deadline {
                    break;
                }
                match pipe.write(next) {
                    Ok(0) => {
                        finished = true;
                        break;
                    }
                    Ok(n) => written += n,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        #[cfg(test)]
                        {
                            INPUT_BLOCKS.with(|slot| slot.set(slot.get() + 1));
                            if let Some(hook) = INPUT_BLOCKED.with(|slot| slot.borrow_mut().take())
                            {
                                hook();
                            }
                        }
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        finished = true;
                        break;
                    }
                }
            }
            if finished {
                input_failed |= finish_input(input_owner.take()).is_err();
                stdin = None;
            }
        }
        for (index, slot) in pipes.iter_mut().enumerate() {
            let Some(pipe) = slot else { continue };
            loop {
                let mut chunk = [0u8; 4096];
                let read = pipe.read(&mut chunk);
                #[cfg(test)]
                let read = READ_ERROR.with(|fault| {
                    if fault.replace(false) {
                        Err(std::io::Error::other("private original output read fault"))
                    } else {
                        read
                    }
                });
                match read {
                    Ok(0) => {
                        *slot = None;
                        break;
                    }
                    Ok(n) => match output {
                        Output::StderrPrefix(bound) => {
                            if index == 1 && kept_output.len() < bound {
                                let keep = n.min(bound - kept_output.len());
                                kept_output.extend_from_slice(&chunk[..keep]);
                            }
                        }
                        Output::Stdout { bytes, too_large } => {
                            if index == 0 {
                                if kept_output.len() + n > bytes {
                                    overflow = Some(too_large);
                                } else {
                                    kept_output.extend_from_slice(&chunk[..n]);
                                }
                            }
                        }
                    },
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        // Its real end can never be observed now.
                        pin_guard();
                        *slot = None;
                        unreadable = true;
                        break;
                    }
                }
            }
        }
        if status.is_none() {
            let observed = child.try_wait();
            #[cfg(test)]
            let observed = WAIT_ERROR.with(|fault| {
                if fault.replace(false)
                    || WAIT_ERROR_AFTER_OUTPUT
                        .with(|armed| !kept_output.is_empty() && armed.replace(false))
                {
                    Err(std::io::Error::other("private original wait fault"))
                } else {
                    observed
                }
            });
            match observed {
                Ok(found) => status = found,
                Err(_) => {
                    let _ = child.kill();
                    if child.wait().is_err() {
                        pin_guard();
                    }
                    break Ended::Unknown(codes.unobserved);
                }
            }
        }
        if let Some(status) = status {
            if pipes.iter().all(Option::is_none) {
                if unreadable {
                    break Ended::Unknown(codes.unreadable);
                }
                if let Some(too_large) = overflow {
                    break Ended::Unknown(too_large);
                }
                break match status.code() {
                    Some(code) => Ended::Exited {
                        code,
                        output: std::mem::take(&mut kept_output),
                    },
                    None => Ended::Unknown(codes.signalled),
                };
            }
        }
        if Instant::now() >= deadline {
            if status.is_none() {
                // The original child alone; never its group or anything else.
                let _ = child.kill();
                if child.wait().is_err() {
                    pin_guard();
                }
                break Ended::Unknown(codes.timed_out);
            }
            break Ended::Unknown(codes.retained);
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // The loop has really ended, including on early exit or deadline. Never
    // let an inherited alias keep this input live after its writer is done.
    input_failed |= finish_input(input_owner.take()).is_err();
    drop(stdin);
    #[cfg(test)]
    OFFERED.with(|offered| offered.set(Some(written)));
    // A descendant may still hold an original output pipe: with a guard, it
    // keeps that guard until the pipe's real end, separately from the result
    // returned here; without one, the pipe closes now.
    if let Some(retain_with) = &kept {
        for pipe in pipes.into_iter().flatten() {
            retain(pipe, retain_with);
        }
    }
    if input_failed {
        return Ended::Unknown(unwritten);
    }
    match ended {
        Ended::Exited { .. } if written < input.as_ref().map_or(0, Stdin::len) => {
            Ended::Unknown(unwritten)
        }
        ended => ended,
    }
}

/// End the actual writer once; a finish failure is never retried or treated
/// as evidence that the frozen input was delivered.
fn finish_input(owner: Option<InputOwner>) -> std::io::Result<()> {
    let Some(owner) = owner else { return Ok(()) };
    #[cfg(test)]
    let owner = {
        let mut owner = owner;
        if let Some(hook) = INPUT_FINISH.with(|hook| hook.borrow_mut().take()) {
            hook(&mut owner);
        }
        owner
    };
    let finished = owner.finish();
    #[cfg(test)]
    if let Err(error) = &finished {
        INPUT_FINISH_ERROR.with(|slot| slot.set(Some(error.raw_os_error().unwrap_or(-1))));
    }
    finished
}

/// A last check in the spawn turn that `deadline` crosses: it notes whether
/// the deadline was still ahead when it ran, then waits past it. The turn
/// is held only until the deadline, never long.
#[cfg(test)]
fn crossing_check(
    deadline: Instant,
    ahead: Arc<std::sync::atomic::AtomicBool>,
) -> impl FnOnce(&mut Command) -> std::io::Result<()> {
    move |_| {
        ahead.store(
            Instant::now() < deadline,
            std::sync::atomic::Ordering::SeqCst,
        );
        while Instant::now() <= deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }
}

/// Runs `attempt` with a short deadline that its last check crosses, until
/// one run's check began before the deadline, and returns that run's
/// result: a turn that came only after the deadline (other tests' spawns)
/// proves nothing about the last check, so it is tried again.
#[cfg(test)]
pub(in crate::desktop) fn crossed<T>(
    mut attempt: impl FnMut(Instant, Box<dyn FnOnce(&mut Command) -> std::io::Result<()>>) -> T,
) -> T {
    for _ in 0..40 {
        let ahead = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let deadline = Instant::now() + Duration::from_millis(50);
        let result = attempt(deadline, Box::new(crossing_check(deadline, ahead.clone())));
        if ahead.load(std::sync::atomic::Ordering::SeqCst) {
            return result;
        }
    }
    panic!("the spawn turn never came before a 50 ms deadline");
}

/// Sleeps until `deadline` has passed.
#[cfg(test)]
pub(in crate::desktop) fn past(deadline: Instant) {
    while Instant::now() <= deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CODES: Codes = Codes {
        unobserved: "test_unobserved",
        unreadable: "test_unreadable",
        signalled: "test_signalled",
        timed_out: "test_timed_out",
        retained: "test_retained",
    };
    pub(super) fn spec(input: &[u8]) -> Spec<'_> {
        Spec {
            custody_env: "HGS_TEST_ORIGINAL_CUSTODY_FD",
            input: Input::Frozen {
                stdin: Stdin::Bytes(input),
                unwritten: "test_unwritten",
            },
            output: Output::Stdout {
                bytes: 4096,
                too_large: "test_too_large",
            },
            codes: TEST_CODES,
            retain: None,
        }
    }
    pub(super) fn custody() -> Arc<InheritedLock> {
        InheritedLock::try_lock(tempfile::tempfile().unwrap()).unwrap()
    }
    fn cat(deadline: Instant, check: impl FnOnce(&mut Command) -> std::io::Result<()>) -> Ended {
        SPAWNED.with(|spawned| spawned.set(None));
        OFFERED.with(|offered| offered.set(None));
        run(
            Path::new("/bin/cat"),
            &[],
            spec(b"private input"),
            &custody(),
            deadline,
            check,
        )
    }

    /// desktop-u's G1 on #1433: the never-renewed deadline is checked last
    /// before the spawn, so a spent budget never creates a child.
    #[test]
    fn a_spent_deadline_starts_no_child() {
        // Already expired at the call.
        let ended = cat(Instant::now() - Duration::from_millis(1), |_| Ok(()));
        assert_eq!(ended, Ended::NotStarted);
        assert_eq!(SPAWNED.with(|spawned| spawned.take()), None);
        // Spent by the last check inside the spawn turn.
        let ended = crossed(|deadline, check| cat(deadline, check));
        assert_eq!(ended, Ended::NotStarted);
        assert_eq!(SPAWNED.with(|spawned| spawned.take()), None);
        // The same for a child that holds no custody.
        SPAWNED.with(|spawned| spawned.set(None));
        let ended = run_unheld(
            Path::new("/bin/cat"),
            &[],
            spec(b"private input"),
            Instant::now() - Duration::from_millis(1),
            |_| Ok(()),
        );
        assert_eq!(ended, Ended::NotStarted);
        assert_eq!(SPAWNED.with(|spawned| spawned.take()), None);
    }

    fn assert_input_deadline(ended: Ended) {
        // Killing the original child can disconnect Darwin's socket before
        // finish. Keep its conservative unwritten result, and accept it only
        // with the actual native ENOTCONN; never infer completed delivery.
        match (
            INPUT_FINISH_ERROR.with(|slot| slot.take()),
            cfg!(target_os = "macos"),
        ) {
            (None, _) => assert_eq!(ended, Ended::Unknown("test_timed_out")),
            (Some(errno), true) => {
                assert_eq!(errno, libc::ENOTCONN);
                assert_eq!(ended, Ended::Unknown("test_unwritten"));
            }
            (Some(errno), false) => panic!("unexpected shutdown errno {errno}"),
        }
    }

    /// A deadline that passes after the spawn offers no input: the child is
    /// killed, and its result stays Unknown.
    #[test]
    fn no_input_is_offered_past_the_deadline() {
        // A budget that outlasts any wait for the spawn turn, spent right
        // after the spawn, outside the turn.
        let deadline = Instant::now() + Duration::from_secs(3);
        AFTER_SPAWN.with(|hook| *hook.borrow_mut() = Some(Box::new(move || past(deadline))));
        let ended = cat(deadline, |_| Ok(()));
        assert_input_deadline(ended);
        assert!(SPAWNED.with(|spawned| spawned.take()).is_some());
        assert_eq!(OFFERED.with(|offered| offered.take()), Some(0));
        assert!(AFTER_SPAWN.with(|hook| hook.borrow().is_none()));
    }

    /// desktop-u's G1 on #1433 R2: a source read that spends the deadline
    /// inside the write loop offers no more input, so the deadline is checked
    /// after the read. A real file and child; the hook only makes the read
    /// slow, and no guard is skipped.
    #[test]
    fn a_slow_input_read_offers_nothing_past_the_deadline() {
        use std::io::Write;
        for (slow, offered) in [(true, 0), (false, 13)] {
            // A budget that outlasts any wait for the spawn turn.
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(b"private input").unwrap();
            let guard: Arc<dyn Send + Sync> = Arc::new(());
            SPAWNED.with(|spawned| spawned.set(None));
            OFFERED.with(|offered| offered.set(None));
            INPUT_READ.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    if slow {
                        past(deadline)
                    }
                }))
            });
            let ended = run(
                Path::new("/bin/cat"),
                &[],
                Spec {
                    input: Input::Frozen {
                        stdin: Stdin::File(&file, 13),
                        unwritten: "test_unwritten",
                    },
                    retain: Some(Retain {
                        guard: &guard,
                        reader: "test-input-read",
                    }),
                    ..spec(b"")
                },
                &custody(),
                deadline,
                |_| Ok(()),
            );
            assert!(SPAWNED.with(|spawned| spawned.take()).is_some());
            assert!(INPUT_READ.with(|hook| hook.borrow().is_none()));
            assert_eq!(OFFERED.with(|o| o.take()), Some(offered), "slow {slow}");
            if offered == 0 {
                assert_input_deadline(ended);
            } else {
                assert_eq!(
                    ended,
                    Ended::Exited {
                        code: 0,
                        output: b"private input".to_vec()
                    }
                );
            }
        }
    }

    /// Within its budget, the same child runs and reads all of its input.
    #[test]
    fn a_child_within_its_deadline_runs() {
        let ended = cat(Instant::now() + Duration::from_secs(20), |_| Ok(()));
        assert_eq!(
            ended,
            Ended::Exited {
                code: 0,
                output: b"private input".to_vec()
            }
        );
        assert_eq!(OFFERED.with(|offered| offered.take()), Some(13));
    }
}

#[cfg(test)]
mod admission_tests;
#[cfg(test)]
mod fork_tests;

#[cfg(test)]
mod spawn_cost;
