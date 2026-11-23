//! Task #12 (P2.1b): the change bus on Linux, the inotify calls only.
//! inotify watches one folder per descriptor, so the root and each kept
//! subfolder get a watch, bounded by `inotify::MAX_WATCHES`; a kept folder
//! that appears is watched when its creation is read. Records are parsed and
//! the folders kept in inotify.rs, which every platform compiles and tests.
//! The listing metadata feed (`listing_metadata/changes.rs`) uses `init`,
//! `add_watch` and `next` with its own namespace rules.
use super::inotify::{self, Step, Tree};
use super::{Batch, Keep, Note, DEBOUNCE};
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

const _: () = assert!(
    libc::IN_MODIFY == inotify::IN_MODIFY
        && libc::IN_ATTRIB == inotify::IN_ATTRIB
        && libc::IN_CLOSE_WRITE == inotify::IN_CLOSE_WRITE
        && libc::IN_MOVED_FROM == inotify::IN_MOVED_FROM
        && libc::IN_MOVED_TO == inotify::IN_MOVED_TO
        && libc::IN_CREATE == inotify::IN_CREATE
        && libc::IN_DELETE == inotify::IN_DELETE
        && libc::IN_DELETE_SELF == inotify::IN_DELETE_SELF
        && libc::IN_MOVE_SELF == inotify::IN_MOVE_SELF
        && libc::IN_UNMOUNT == inotify::IN_UNMOUNT
        && libc::IN_Q_OVERFLOW == inotify::IN_Q_OVERFLOW
        && libc::IN_IGNORED == inotify::IN_IGNORED
        && libc::IN_ISDIR == inotify::IN_ISDIR
        && std::mem::size_of::<libc::inotify_event>() == inotify::HEADER
);

/// What the bus asks of each folder.
const MASK: u32 = libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_MODIFY
    | libc::IN_CLOSE_WRITE
    | libc::IN_ATTRIB
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_ONLYDIR
    | libc::IN_DONT_FOLLOW;
/// Records read per wake before a due hint is delivered.
const READS: usize = 4096;

/// A non-blocking, close-on-exec inotify descriptor.
pub(crate) fn init() -> io::Result<OwnedFd> {
    // SAFETY: plain call; a valid descriptor is owned below.
    let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: inotify_init1 returned a descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

pub(crate) fn add_watch(fd: RawFd, path: &CString, mask: u32) -> io::Result<i32> {
    // SAFETY: `path` is NUL-terminated and lives during the call.
    let wd = unsafe { libc::inotify_add_watch(fd, path.as_ptr(), mask) };
    if wd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(wd)
    }
}

/// The next record, reading more from `fd` when `buffer[*offset..*length]`
/// is used up; None when nothing more is queued.
pub(crate) fn next(fd: RawFd, buffer: &mut [u8], offset: &mut usize, length: &mut usize) -> io::Result<Option<inotify::Raw>> {
    if *offset >= *length {
        // SAFETY: `buffer` is valid for `buffer.len()` bytes.
        let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if read < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            return Err(error);
        }
        if read == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the change feed ended"));
        }
        *offset = 0;
        *length = read as usize;
    }
    inotify::parse_next(&buffer[..*length], offset)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// The descriptor and the folders it watches.
struct Feed {
    fd: OwnedFd,
    root: PathBuf,
    keep: Keep,
    tree: Tree,
}

impl Feed {
    /// Watches `relative` and its kept subfolders.
    fn watch(&mut self, relative: &Path) -> Result<(), String> {
        let mut pending = vec![relative.to_path_buf()];
        while let Some(folder) = pending.pop() {
            let path = self.root.join(&folder);
            let name = CString::new(path.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
            let wd = match add_watch(self.fd.as_raw_fd(), &name, MASK) {
                Ok(wd) => wd,
                Err(error) if error.raw_os_error() == Some(libc::ENOSPC) => {
                    return Err("the inotify watch limit is exhausted".into());
                }
                // A subfolder removed or replaced meanwhile: its parent's
                // record names it.
                Err(error)
                    if !folder.as_os_str().is_empty()
                        && matches!(error.raw_os_error(), Some(libc::ENOENT) | Some(libc::ENOTDIR)) =>
                {
                    continue
                }
                Err(error) => return Err(format!("cannot watch {}: {error}", path.display())),
            };
            self.tree.watched(wd, folder.clone())?;
            let Ok(entries) = std::fs::read_dir(&path) else { continue };
            for entry in entries.flatten() {
                // A symbolic link is never followed.
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    let child = folder.join(entry.file_name());
                    if (self.keep)(&child) {
                        pending.push(child);
                    }
                }
            }
        }
        Ok(())
    }
}

/// A running watcher thread; dropping it stops the thread and waits for it.
pub(super) struct Watcher {
    stop: Arc<OwnedFd>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    pub(super) fn start(root: &Path, keep: Keep, buffer: usize, sink: impl FnMut(Note) -> bool + Send + 'static) -> Result<Self, String> {
        let fd = init().map_err(|error| format!("cannot start inotify: {error}"))?;
        let mut feed = Feed { fd, root: root.to_path_buf(), keep, tree: Tree::default() };
        // Armed before the thread starts: a change after `start` is queued.
        feed.watch(Path::new(""))?;
        // SAFETY: plain call; a valid descriptor is owned below.
        let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if raw < 0 {
            return Err(format!("cannot create the stop event: {}", io::Error::last_os_error()));
        }
        // SAFETY: eventfd returned a descriptor that nothing else owns.
        let stop = Arc::new(unsafe { OwnedFd::from_raw_fd(raw) });
        let stopping = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("hgs-change-bus".into())
            .spawn(move || run(feed, &stopping, buffer, sink))
            .map_err(|error| format!("cannot start the change watcher: {error}"))?;
        Ok(Self { stop, thread: Some(thread) })
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let one: u64 = 1;
        // SAFETY: the eventfd is open while `self.stop` lives; 8 bytes are written.
        unsafe { libc::write(self.stop.as_raw_fd(), (&one as *const u64).cast(), 8) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Delivers what was gathered, then the failure.
fn fail(batch: &mut Batch, sink: &mut impl FnMut(Note) -> bool, error: String) {
    if let Some(hint) = batch.take() {
        sink(Note::Hint(hint));
    }
    let _ = sink(Note::Failed(format!("the change watcher stopped: {error}")));
}

fn run(mut feed: Feed, stop: &OwnedFd, buffer: usize, mut sink: impl FnMut(Note) -> bool) {
    let mut bytes = vec![0u8; buffer.max(inotify::HEADER + 256)];
    let (mut offset, mut length) = (0, 0);
    let mut batch = Batch::default();
    let mut flush_at: Option<Instant> = None;
    loop {
        let timeout = flush_at.map_or(-1, |at| {
            at.saturating_duration_since(Instant::now()).as_millis().min(i32::MAX as u128) as i32
        });
        let mut fds = [
            libc::pollfd { fd: feed.fd.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: stop.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        // SAFETY: `fds` holds two valid entries during the call.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return fail(&mut batch, &mut sink, error.to_string());
        }
        if fds[1].revents != 0 {
            return;
        }
        if fds[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return fail(&mut batch, &mut sink, "the inotify descriptor failed".into());
        }
        if fds[0].revents & libc::POLLIN != 0 {
            for _ in 0..READS {
                let event = match next(feed.fd.as_raw_fd(), &mut bytes, &mut offset, &mut length) {
                    Ok(Some(event)) => event,
                    Ok(None) => break,
                    Err(error) => return fail(&mut batch, &mut sink, error.to_string()),
                };
                let keep = feed.keep;
                match feed.tree.event(event.wd, event.mask, &bytes[event.name.clone()], keep, &mut batch) {
                    Step::Nothing => {}
                    Step::Watch(folder) => {
                        if let Err(error) = feed.watch(&folder) {
                            return fail(&mut batch, &mut sink, error);
                        }
                    }
                    Step::Forget(wd) => {
                        // SAFETY: plain call on the open descriptor.
                        unsafe { libc::inotify_rm_watch(feed.fd.as_raw_fd(), wd) };
                    }
                    Step::Fail(error) => return fail(&mut batch, &mut sink, error),
                }
            }
            flush_at.get_or_insert_with(|| Instant::now() + DEBOUNCE);
        }
        if flush_at.is_some_and(|at| Instant::now() >= at) {
            flush_at = None;
            if let Some(hint) = batch.take() {
                if !sink(Note::Hint(hint)) {
                    return;
                }
            }
        }
    }
}

/// Written for Linux and not yet compiled or run there (task #12): this
/// computer has no Linux; the upstream PR's Linux CI runs them.
#[cfg(test)]
mod tests {
    use super::super::{Bus, Hint};
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;
    use std::sync::mpsc;
    use std::time::Duration;

    fn no_temporary(path: &Path) -> bool {
        path.file_name().and_then(|name| name.to_str()).is_some_and(|name| !name.starts_with(".hgs-"))
    }

    fn listen(root: &Path) -> (Bus, mpsc::Receiver<Note>) {
        let (notes, received) = mpsc::channel();
        let bus = Bus::start(root, no_temporary, move |note| notes.send(note).is_ok()).unwrap();
        (bus, received)
    }

    /// Hints until one names `path` (relative), within 2 s; the paths seen.
    fn hinted(received: &mpsc::Receiver<Note>, path: &Path) -> BTreeSet<PathBuf> {
        let until = Instant::now() + Duration::from_secs(2);
        let mut seen = BTreeSet::new();
        while let Ok(note) = received.recv_timeout(until.saturating_duration_since(Instant::now())) {
            match note {
                Note::Hint(hint) => {
                    assert!(!hint.overflow, "no overflow for a few files");
                    seen.extend(hint.paths);
                    if seen.contains(path) {
                        return seen;
                    }
                }
                Note::Failed(error) => panic!("{error}"),
            }
        }
        panic!("no hint for {}; seen {seen:?}", path.display());
    }

    #[test]
    fn create_replace_by_rename_delete_and_a_new_subfolder_are_hinted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("bindings")).unwrap();
        let (bus, received) = listen(root);

        fs::write(root.join("a.json"), "{}").unwrap();
        hinted(&received, Path::new("a.json"));

        fs::write(root.join(".hgs-next"), "[]").unwrap();
        fs::rename(root.join(".hgs-next"), root.join("a.json")).unwrap();
        let seen = hinted(&received, Path::new("a.json"));
        assert!(!seen.contains(Path::new(".hgs-next")), "{seen:?}");

        fs::write(root.join("bindings").join("b.json"), "{}").unwrap();
        hinted(&received, &Path::new("bindings").join("b.json"));

        fs::remove_file(root.join("a.json")).unwrap();
        hinted(&received, Path::new("a.json"));

        // A folder made after the start is watched from its creation on.
        fs::create_dir(root.join("labels")).unwrap();
        hinted(&received, Path::new("labels"));
        fs::write(root.join("labels").join("l.json"), "{}").unwrap();
        hinted(&received, &Path::new("labels").join("l.json"));

        fs::write(root.join(".hgs-other"), "x").unwrap();
        assert!(received.recv_timeout(Duration::from_millis(300)).is_err(), "a temporary file wakes nobody");

        drop(bus);
        fs::write(root.join("c.json"), "{}").unwrap();
        assert!(received.recv_timeout(Duration::from_millis(300)).is_err(), "a stopped watcher sends nothing");
    }

    /// 10 000 files while the receiver does not read: the kernel queue
    /// (16 384 records by default) overflows, and the watcher says so.
    #[test]
    fn an_overflow_is_a_hint_that_names_everything() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (notes, received) = mpsc::sync_channel::<Note>(0);
        let bus = Bus::start(root, no_temporary, move |note| notes.send(note).is_ok()).unwrap();
        for index in 0..10_000 {
            fs::write(root.join(format!("file-{index:05}.json")), "x").unwrap();
        }
        let until = Instant::now() + Duration::from_secs(20);
        let mut overflow = false;
        while let Ok(note) = received.recv_timeout(until.saturating_duration_since(Instant::now())) {
            match note {
                Note::Hint(Hint { overflow: true, paths }) => {
                    assert!(paths.is_empty());
                    overflow = true;
                    break;
                }
                Note::Hint(_) => {}
                Note::Failed(error) => panic!("{error}"),
            }
        }
        assert!(overflow, "10 000 files overflow the queue");
        drop(received);
        drop(bus);
    }

    #[test]
    fn a_missing_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let error = Bus::start(&dir.path().join("missing"), no_temporary, |_| true).err().unwrap();
        assert!(error.contains("cannot watch"), "{error}");
    }
}
