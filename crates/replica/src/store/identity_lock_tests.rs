use super::fork_tests::{pipe, Forked};
use super::{ProjectStore, LOCK_ACQUIRED, STORE_ID_CREATING};
use fs2::FileExt;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::mpsc;

fn inherited_contender(path: &Path) -> (Forked, File, File) {
    // This contender has already observed the live owner's lock before the
    // identity is published (or construction returns). Keep its original fd.
    let contender = File::open(path).unwrap();
    assert!(contender.try_lock_exclusive().is_err());
    let (mut ready_read, ready_write) = pipe();
    let (gate_read, gate_write) = pipe();
    let fds = [
        ready_read.as_raw_fd(),
        ready_write.as_raw_fd(),
        gate_read.as_raw_fd(),
        gate_write.as_raw_fd(),
    ];
    let pid = std::thread::spawn(move || unsafe {
        let pid = libc::fork();
        if pid == 0 {
            // No Rust drops or allocations: only async-signal-safe calls.
            libc::close(fds[0]);
            libc::close(fds[3]);
            let mut byte = 1_u8;
            if libc::write(fds[1], (&byte as *const u8).cast(), 1) != 1 {
                libc::_exit(120);
            }
            if libc::read(fds[2], (&mut byte as *mut u8).cast(), 1) != 1 {
                libc::_exit(121);
            }
            libc::_exit(0);
        }
        pid
    })
    .join()
    .unwrap();
    let child = Forked::new(pid);
    drop(ready_write);
    drop(gate_read);
    ready_read.read_exact(&mut [0]).unwrap();
    (child, gate_write, contender)
}

fn released((mut child, mut gate, contender): (Forked, File, File)) {
    let result = contender.try_lock_exclusive();
    assert!(
        result.is_ok(),
        "a returned replica scope must release its lock while the inherited child fd stays open: {result:?}"
    );
    gate.write_all(&[1]).unwrap();
    child.wait();
}

fn identity_recheck(bytes: Option<&'static [u8]>) {
    let root = tempfile::tempdir().unwrap();
    let identity = root.path().join("store-id");
    let at_hook = identity.clone();
    let (send, received) = mpsc::sync_channel(1);
    LOCK_ACQUIRED.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |path| {
            send.send(inherited_contender(path)).unwrap();
            if let Some(bytes) = bytes {
                fs::write(&at_hook, bytes).unwrap();
            }
        }))
    });
    let result = ProjectStore::store_id(root.path());
    LOCK_ACQUIRED.with(|hook| hook.borrow_mut().take());
    let held = received.recv().unwrap();
    match bytes {
        None => assert!(super::ids::is_uuid(&result.unwrap())),
        Some(bytes) if bytes == b"invalid" => {
            assert!(result.is_err());
            assert_eq!(fs::read(&identity).unwrap(), bytes);
        }
        Some(bytes) => assert_eq!(result.unwrap().as_bytes(), bytes),
    }
    released(held);
}

#[test]
fn identity_publication_releases_the_already_contended_lock_before_child_exec() {
    identity_recheck(None);
}

#[test]
fn an_identity_found_after_acquisition_releases_the_lock_on_early_success() {
    identity_recheck(Some(b"11111111-1111-4111-8111-111111111111"));
}

#[test]
fn an_invalid_identity_found_after_acquisition_releases_the_lock_on_error() {
    identity_recheck(Some(b"invalid"));
}

#[test]
fn failed_identity_publication_releases_the_lock_before_child_exec() {
    let root = tempfile::tempdir().unwrap();
    let identity = root.path().join("store-id");
    let (send, received) = mpsc::sync_channel(1);
    let path = root.path().join("store-id.lock");
    STORE_ID_CREATING.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            send.send(inherited_contender(&path)).unwrap();
            // A real rename error, after the identity recheck; no permission fake.
            fs::create_dir(&identity).unwrap();
        }))
    });
    let result = ProjectStore::store_id(root.path());
    STORE_ID_CREATING.with(|hook| hook.borrow_mut().take());
    let held = received.recv().unwrap();
    assert!(result.is_err());
    released(held);
}

fn failed_open(copy_review: bool) {
    let root = tempfile::tempdir().unwrap();
    if copy_review {
        drop(ProjectStore::open(root.path(), "error-path").unwrap());
    }
    let (send, received) = mpsc::sync_channel(1);
    LOCK_ACQUIRED.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |path| {
            send.send(inherited_contender(path)).unwrap();
            let tasks = path.parent().unwrap().join("tasks");
            fs::remove_dir(&tasks).unwrap();
            File::create(tasks).unwrap();
        }))
    });
    let result = if copy_review {
        ProjectStore::open_copy_review(root.path(), "error-path", std::time::Duration::from_secs(1))
    } else {
        ProjectStore::open(root.path(), "error-path").map(Some)
    };
    LOCK_ACQUIRED.with(|hook| hook.borrow_mut().take());
    let held = received.recv().unwrap();
    assert!(result.is_err());
    released(held);
}

#[test]
fn failed_project_construction_releases_its_acquired_lock_before_child_exec() {
    failed_open(false);
}

#[test]
fn failed_copy_review_construction_releases_its_acquired_lock_before_child_exec() {
    failed_open(true);
}

#[test]
fn dropping_a_child_scope_guard_cannot_unlock_its_live_parent() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("lock");
    let file = File::create(&path).unwrap();
    file.lock_exclusive().unwrap();
    let guard = super::OwnedLock::acquired(file);
    let contender = File::open(&path).unwrap();
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        drop(guard);
        unsafe { libc::_exit(0) };
    }
    let mut child = Forked::new(pid);
    child.wait();
    assert!(
        contender.try_lock_exclusive().is_err(),
        "a fork child cannot release the parent's acquired scope lock"
    );
    drop(guard);
    assert!(contender.try_lock_exclusive().is_ok());
}
