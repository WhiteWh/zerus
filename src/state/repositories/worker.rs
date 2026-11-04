//! Unix Git has a deadline owner independent of the disposable GUI/SSH wrapper.
//! The wrapper holds the worker's stdin open. EOF cancels only its Git group;
//! Git receives /dev/null, so it cannot inherit and retain that lifeline.
use super::*;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::Stdio;

#[cfg(test)]
thread_local! {
    pub(super) static STARTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// A denial-only reentrancy marker inherited by Git and its descendants. It is
// never evidence of permission or a substitute for the original image proof.
pub(super) const CONTEXT: &str = "HGS_REPOSITORY_GIT_WORKER";

fn wrapper_context(test_image: bool, nested: bool) -> Result<()> {
    if test_image {
        return Err("A unit-test image cannot launch a repository Git worker".into());
    }
    if nested {
        return Err("Nested repository Git workers are refused".into());
    }
    Ok(())
}

fn worker_arguments(mut args: impl Iterator<Item = std::ffi::OsString>) -> bool {
    args.next().is_some_and(|v| v == "__state")
        && args.next().is_some_and(|v| v == "repository-git-worker")
}

fn image_proof(identity: crate::platform::FileIdentity) -> String {
    format!(
        "{}:{}:{}:{}",
        identity.device, identity.inode, identity.modified, identity.modified_nanos
    )
}

fn admit_image(
    running: Option<crate::platform::FileIdentity>,
    candidate: Option<crate::platform::FileIdentity>,
) -> Result<String> {
    let running = running.ok_or("Cannot verify the running hgs image")?;
    let candidate = candidate.ok_or("Cannot verify the Git worker executable")?;
    if running != candidate {
        return Err("hgs changed during acquisition; reopen the dialog".into());
    }
    Ok(image_proof(running))
}

fn verify_image(expected: &str, actual: Option<crate::platform::FileIdentity>) -> Result<()> {
    let actual = actual.ok_or("Cannot verify the running Git worker image")?;
    if image_proof(actual) != expected {
        return Err("Git worker image changed before launch; reopen the dialog".into());
    }
    Ok(())
}

pub(super) fn run(
    directory: &directory::Directory,
    args: &[&str],
    seconds: u64,
    control: Option<&Control<'_>>,
) -> Result<Vec<u8>> {
    #[cfg(test)]
    STARTS.with(|count| count.set(count.get() + 1));
    // libtest replaces main: re-executing a unit-test image with worker argv
    // would interpret "clone" as a test filter and run those tests recursively.
    wrapper_context(
        cfg!(test),
        std::env::var_os(CONTEXT).is_some() || worker_arguments(std::env::args_os().skip(1)),
    )?;
    #[cfg(target_os = "linux")]
    let executable = PathBuf::from("/proc/self/exe");
    #[cfg(not(target_os = "linux"))]
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let proof = admit_image(
        crate::platform::image_identity(),
        crate::platform::file_identity(&executable),
    )?;
    let mut command = Command::new(executable);
    command
        .args([
            "__state",
            "repository-git-worker",
            &seconds.to_string(),
            &proof,
        ])
        .args(args)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    directory.anchor(&mut command)?;
    let mut child = {
        let _turn = crate::platform::spawn_turn();
        if let Some(control) = control {
            // Final hook: only this exact admitted lock survives exec. The Git
            // deadline owner and its descendants retain it after service exit.
            control
                .custody
                .inherit(&mut command)
                .map_err(|e| e.to_string())?;
        }
        command
            .spawn()
            .map_err(|e| format!("Cannot start Git worker: {e}"))?
    };
    let lifeline = child.stdin.take();
    let mut encoded = Vec::new();
    // Git's two 64 KiB byte arrays / escaped error fit within this envelope.
    let read = with_lifeline(lifeline, control.map(|v| v.cancelled), || {
        child
            .stdout
            .take()
            .unwrap()
            .take(524_289)
            .read_to_end(&mut encoded)
    });
    let status = child
        .wait()
        .map_err(|e| format!("Cannot wait for Git worker: {e}"))?;
    read.map_err(|e| format!("Cannot read Git worker: {e}"))?;
    if !status.success() || encoded.len() > 524_288 {
        return Err("Git worker interrupted; inspect the destination before continuing".into());
    }
    serde_json::from_slice::<Result<Vec<u8>>>(&encoded)
        .map_err(|_| "Invalid Git worker result".to_string())?
}

/// Cancellation closes only the original worker's input. Its deadline owner
/// remains retained until output EOF and real exit; it is never killed merely
/// because the actor asked to drain. Ordinary CLI acquisition needs no watcher.
fn with_lifeline<T>(
    lifeline: Option<std::process::ChildStdin>,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
    read: impl FnOnce() -> T,
) -> T {
    let Some(cancelled) = cancelled else {
        let result = read();
        drop(lifeline);
        return result;
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    let finished = AtomicBool::new(false);
    struct Finished<'a>(&'a AtomicBool);
    impl Drop for Finished<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    std::thread::scope(|scope| {
        let finished = &finished;
        let watch = scope.spawn(move || {
            while !finished.load(Ordering::Acquire) && !cancelled.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(5));
            }
            drop(lifeline);
        });
        let completion = Finished(finished);
        let result = read();
        drop(completion);
        watch.join().expect("owned Git lifeline watcher panicked");
        result
    })
}

pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    // Git must not re-enter the worker, even through a script that execs hgs.
    if std::env::var_os(CONTEXT).is_some() {
        return Err("Nested repository Git workers are refused".into());
    }
    let seconds: u64 = args
        .first()
        .ok_or("Missing Git deadline")?
        .parse()
        .map_err(|_| "Invalid Git deadline")?;
    if !(1..=600).contains(&seconds) || args.len() < 3 {
        return Err("Invalid Git worker arguments".into());
    }
    // The executable pathname can change after the parent's check. Bind this
    // actual running image before any Git work, including on macOS upgrades.
    // Unknown image evidence never counts as equality.
    verify_image(&args[1], crate::platform::image_identity())?;
    // Refuse invocation without a wrapper-owned pipe. This is an internal ABI,
    // never an alternative public route for unbounded interactive Git.
    unsafe {
        let mut stat: libc::stat = std::mem::zeroed();
        if libc::fstat(0, &mut stat) != 0 || stat.st_mode & libc::S_IFMT != libc::S_IFIFO {
            return Err("Git worker requires a wrapper lifeline".into());
        }
        let flags = libc::fcntl(0, libc::F_GETFL);
        if flags < 0 || libc::fcntl(0, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err("Cannot watch Git wrapper lifetime".into());
        }
    }
    let cancelled = || unsafe {
        let mut byte = 0u8;
        match libc::read(0, (&mut byte as *mut u8).cast(), 1) {
            0 => true,
            -1 => !matches!(
                std::io::Error::last_os_error().kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ),
            _ => true, // The protocol carries no input.
        }
    };
    let refs: Vec<&str> = args[2..].iter().map(String::as_str).collect();
    let result = run_git(None, &refs, seconds, &cancelled);
    // All Git work is finished before writing. A dead reader cannot leave an
    // operation running while this worker blocks or exits on a broken pipe.
    let encoded = serde_json::to_vec(&result).map_err(|e| e.to_string())?;
    std::io::stdout()
        .write_all(&encoded)
        .map_err(|e| e.to_string())?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_images_and_nested_contexts_refuse_before_any_self_exec() {
        assert!(wrapper_context(true, false).is_err());
        assert!(wrapper_context(true, true).is_err());
        assert!(wrapper_context(false, true).is_err());
        assert!(wrapper_context(false, false).is_ok());
        let args = ["__state", "repository-git-worker", "600", "proof", "clone"];
        assert!(worker_arguments(args.into_iter().map(Into::into)));
        assert!(!worker_arguments(
            ["repository", "clone"].into_iter().map(Into::into)
        ));
    }

    #[test]
    fn clone_worker_entry_in_a_unit_image_is_always_refused() {
        let root = tempfile::tempdir().unwrap();
        let directory = directory::Directory::open(&root.path().canonicalize().unwrap()).unwrap();
        let error = run(&directory, &["clone"], 600, None).unwrap_err();
        assert_eq!(
            error,
            "A unit-test image cannot launch a repository Git worker"
        );
    }

    #[test]
    fn actor_cancellation_closes_the_lifeline_without_releasing_the_original_reader() {
        use std::{
            os::fd::AsRawFd,
            sync::atomic::{AtomicBool, Ordering},
        };
        let mut command = Command::new("python3");
        command.args(["-c", "import sys\nsys.stdout.write('ready');sys.stdout.flush()\nsys.stdin.buffer.read()\nsys.stdout.write('closed');sys.stdout.flush()"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = crate::platform::spawn_in_turn(&mut command).unwrap();
        let input = child.stdin.take();
        let mut output = child.stdout.take().unwrap();
        let fd = output.as_raw_fd();
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            assert!(flags >= 0);
            assert_eq!(libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK), 0);
        }
        let cancelled = AtomicBool::new(false);
        let mut bytes = Vec::new();
        let eof = with_lifeline(input, Some(&cancelled), || {
            let until = Instant::now() + Duration::from_secs(4);
            loop {
                let mut chunk = [0; 64];
                match output.read(&mut chunk) {
                    Ok(0) => return true,
                    Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    other => panic!("{other:?}"),
                }
                if bytes.starts_with(b"ready") {
                    cancelled.store(true, Ordering::Release);
                }
                if Instant::now() >= until {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        assert!(child.wait().unwrap().success());
        assert!(
            eof,
            "the original reader did not observe EOF after actor cancellation"
        );
        assert_eq!(bytes, b"readyclosed");
    }

    fn identity() -> crate::platform::FileIdentity {
        crate::platform::FileIdentity {
            device: 1,
            inode: 2,
            modified: 3,
            modified_nanos: 4,
        }
    }

    #[test]
    fn image_admission_requires_both_known_identities() {
        let id = identity();
        assert!(admit_image(None, None).is_err());
        assert!(admit_image(Some(id), None).is_err());
        assert!(admit_image(None, Some(id)).is_err());
        assert_eq!(admit_image(Some(id), Some(id)).unwrap(), image_proof(id));
    }

    #[test]
    fn image_admission_compares_the_complete_identity() {
        let id = identity();
        for other in [
            crate::platform::FileIdentity { device: 5, ..id },
            crate::platform::FileIdentity { inode: 5, ..id },
            crate::platform::FileIdentity { modified: 5, ..id },
            crate::platform::FileIdentity {
                modified_nanos: 5,
                ..id
            },
        ] {
            assert!(admit_image(Some(id), Some(other)).is_err());
        }
    }

    #[test]
    fn worker_rechecks_known_actual_image_before_git() {
        let id = identity();
        let proof = image_proof(id);
        assert!(verify_image("", None).is_err());
        assert!(verify_image(&proof, None).is_err());
        assert!(verify_image(
            &proof,
            Some(crate::platform::FileIdentity { inode: 9, ..id })
        )
        .is_err());
        verify_image(&proof, Some(id)).unwrap();
    }
}
