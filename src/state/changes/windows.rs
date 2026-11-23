//! The change bus on Windows: `ReadDirectoryChangesW` on one folder and its
//! subfolders, overlapped, on a thread of its own. A burst is delivered once
//! after [`DEBOUNCE`]; a full buffer (`ERROR_NOTIFY_ENUM_DIR`, or a completed
//! read of no bytes) is an overflow hint; any other error ends the watcher
//! with [`Note::Failed`]. Network folders are refused: their notifications
//! are not reliable, so the caller keeps its pulse there.
use super::{Batch, Keep, Note, DEBOUNCE};
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use windows_sys::Win32::{
    Foundation::{
        GetLastError, ERROR_NOTIFY_ENUM_DIR, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Storage::FileSystem::{
        CreateFileW, GetDriveTypeW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED,
        FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
        FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    },
    System::{
        Threading::{CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects, INFINITE},
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
    },
};

/// `GetDriveTypeW` for a network drive.
const DRIVE_REMOTE: u32 = 4;
const FILTER: u32 = FILE_NOTIFY_CHANGE_FILE_NAME
    | FILE_NOTIFY_CHANGE_DIR_NAME
    | FILE_NOTIFY_CHANGE_SIZE
    | FILE_NOTIFY_CHANGE_LAST_WRITE
    | FILE_NOTIFY_CHANGE_CREATION;
/// How long `start` waits for the watcher to arm.
const ARM: Duration = Duration::from_secs(5);

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    handle.as_raw_handle() as HANDLE
}

/// A manual-reset event.
struct Event(OwnedHandle);

impl Event {
    fn new() -> Result<Self, String> {
        // SAFETY: no security attributes and no name; the call creates a new object.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(format!("cannot create an event: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: CreateEventW returned a handle that nothing else owns.
        Ok(Self(unsafe { OwnedHandle::from_raw_handle(event) }))
    }

    fn raw(&self) -> HANDLE {
        raw(&self.0)
    }
}

/// A network folder: `\\server\share`, or a drive letter mapped to one.
fn remote(root: &Path) -> bool {
    let shown = root.as_os_str().to_string_lossy();
    let local = shown.strip_prefix(r"\\?\").unwrap_or(&shown);
    if local.starts_with(r"\\") || local.starts_with("UNC\\") {
        return true;
    }
    let bytes = local.as_bytes();
    if bytes.len() < 2 || bytes[1] != b':' {
        return false;
    }
    let drive: Vec<u16> = format!("{}:\\", bytes[0] as char).encode_utf16().chain(Some(0)).collect();
    // SAFETY: `drive` is a NUL-terminated wide string.
    unsafe { GetDriveTypeW(drive.as_ptr()) == DRIVE_REMOTE }
}

/// A running watcher thread; dropping it stops the thread and waits for it.
pub(super) struct Watcher {
    stop: Arc<Event>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    pub(super) fn start(root: &Path, keep: Keep, buffer: usize, sink: impl FnMut(Note) -> bool + Send + 'static) -> Result<Self, String> {
        if remote(root) {
            return Err(format!("{} is a network folder", root.display()));
        }
        let name = wide(root);
        // SAFETY: `name` is a NUL-terminated wide string; no security attributes or template.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(format!("cannot watch {}: {}", root.display(), std::io::Error::last_os_error()));
        }
        // SAFETY: CreateFileW returned a handle that nothing else owns.
        let directory = unsafe { OwnedHandle::from_raw_handle(handle) };
        let stop = Arc::new(Event::new()?);
        let stopping = Arc::clone(&stop);
        let (ready, armed) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("hgs-change-bus".into())
            .spawn(move || watch(directory, &stopping, keep, buffer, sink, ready))
            .map_err(|error| format!("cannot start the change watcher: {error}"))?;
        let watcher = Self { stop, thread: Some(thread) };
        match armed.recv_timeout(ARM) {
            Ok(Ok(())) => Ok(watcher),
            Ok(Err(error)) => Err(error),
            Err(_) => Err("the change watcher did not start".into()),
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // SAFETY: the event is open while `self.stop` lives.
        unsafe { SetEvent(self.stop.raw()) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts the next read into `buffer`; `overlapped` signals its event.
fn issue(handle: HANDLE, buffer: &mut [u32], overlapped: &mut OVERLAPPED) -> Result<(), String> {
    // SAFETY: the event is open; resetting it before a new read.
    unsafe { ResetEvent(overlapped.hEvent) };
    // SAFETY: `buffer` and `overlapped` stay in place and unused until the
    // read completes or is cancelled and waited for (see `cancel`).
    let started = unsafe {
        ReadDirectoryChangesW(
            handle,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 4) as u32,
            1,
            FILTER,
            std::ptr::null_mut(),
            overlapped,
            None,
        )
    };
    if started == 0 {
        return Err(format!("cannot read changes: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Cancels the pending read and waits until the system no longer uses its
/// buffer and OVERLAPPED.
fn cancel(handle: HANDLE, overlapped: &OVERLAPPED) {
    let mut count = 0;
    // SAFETY: `overlapped` belongs to the pending read on `handle`; the
    // waiting call returns only once that read ended.
    unsafe {
        CancelIoEx(handle, overlapped);
        GetOverlappedResult(handle, overlapped, &mut count, 1);
    }
}

/// Adds the names of `FILE_NOTIFY_INFORMATION` records to `batch`. A record
/// that does not fit is treated as an overflow: everything is scanned.
fn parse(bytes: &[u8], keep: Keep, batch: &mut Batch) {
    let field = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let mut offset = 0;
    loop {
        let (Some(next), Some(length)) = (field(offset), field(offset + 8)) else { return batch.overflow() };
        let start = offset + 12;
        let Some(name) = bytes.get(start..start + length).filter(|_| length % 2 == 0) else { return batch.overflow() };
        let name: Vec<u16> = name.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        batch.add(PathBuf::from(OsString::from_wide(&name)), keep);
        if next == 0 {
            return;
        }
        offset += next;
    }
}

fn watch(
    directory: OwnedHandle,
    stop: &Event,
    keep: Keep,
    buffer: usize,
    mut sink: impl FnMut(Note) -> bool,
    ready: mpsc::Sender<Result<(), String>>,
) {
    let handle = raw(&directory);
    let io = match Event::new() {
        Ok(io) => io,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    // u32 words: the records are DWORD-aligned.
    let mut words = vec![0u32; buffer.div_ceil(4)];
    let mut overlapped = OVERLAPPED { hEvent: io.raw(), ..Default::default() };
    if let Err(error) = issue(handle, &mut words, &mut overlapped) {
        let _ = ready.send(Err(error));
        return;
    }
    let _ = ready.send(Ok(()));
    let mut batch = Batch::default();
    let mut flush_at: Option<Instant> = None;
    loop {
        let timeout = flush_at.map_or(INFINITE, |at| {
            u32::try_from(at.saturating_duration_since(Instant::now()).as_millis()).unwrap_or(INFINITE - 1)
        });
        let events = [io.raw(), stop.raw()];
        // SAFETY: both events stay open during the wait.
        let woke = unsafe { WaitForMultipleObjects(2, events.as_ptr(), 0, timeout) };
        if woke == WAIT_OBJECT_0 {
            let mut count = 0u32;
            // SAFETY: the read signalled its event, so it completed; no waiting.
            if unsafe { GetOverlappedResult(handle, &overlapped, &mut count, 0) } != 0 {
                if count == 0 {
                    // The system's buffer overflowed: the records are lost.
                    batch.overflow();
                } else {
                    let length = (count as usize).min(words.len() * 4);
                    // SAFETY: the completed read wrote `count` bytes into `words`,
                    // which nothing else uses until the next `issue`.
                    let bytes = unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), length) };
                    parse(bytes, keep, &mut batch);
                }
            } else {
                // SAFETY: plain call right after the failure.
                let error = unsafe { GetLastError() };
                if error == ERROR_NOTIFY_ENUM_DIR {
                    batch.overflow();
                } else {
                    let _ = sink(Note::Failed(format!("the change watcher stopped: {}", std::io::Error::from_raw_os_error(error as i32))));
                    return;
                }
            }
            flush_at.get_or_insert_with(|| Instant::now() + DEBOUNCE);
            // Read on before delivering, so the system keeps recording.
            if let Err(error) = issue(handle, &mut words, &mut overlapped) {
                if let Some(hint) = batch.take() {
                    sink(Note::Hint(hint));
                }
                let _ = sink(Note::Failed(error));
                return;
            }
        } else if woke == WAIT_OBJECT_0 + 1 {
            cancel(handle, &overlapped);
            return;
        } else if woke != WAIT_TIMEOUT {
            let error = std::io::Error::last_os_error();
            cancel(handle, &overlapped);
            let _ = sink(Note::Failed(format!("the change watcher stopped: {error}")));
            return;
        }
        if flush_at.is_some_and(|at| Instant::now() >= at) {
            flush_at = None;
            if let Some(hint) = batch.take() {
                if !sink(Note::Hint(hint)) {
                    cancel(handle, &overlapped);
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Bus, Hint};
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;

    fn no_temporary(path: &Path) -> bool {
        path.file_name().and_then(|name| name.to_str()).is_some_and(|name| !name.starts_with(".hgs-"))
    }

    fn listen(root: &Path) -> (Bus, mpsc::Receiver<Note>) {
        let (notes, received) = mpsc::channel();
        let bus = Bus::start(root, no_temporary, move |note| notes.send(note).is_ok()).unwrap();
        (bus, received)
    }

    /// Hints until one names `path` (relative), within 2 s; the paths seen.
    fn hinted(received: &mpsc::Receiver<Note>, path: &str) -> BTreeSet<PathBuf> {
        let until = Instant::now() + Duration::from_secs(2);
        let mut seen = BTreeSet::new();
        while let Ok(note) = received.recv_timeout(until.saturating_duration_since(Instant::now())) {
            match note {
                Note::Hint(hint) => {
                    assert!(!hint.overflow, "no overflow for a few files");
                    seen.extend(hint.paths);
                    if seen.contains(Path::new(path)) {
                        return seen;
                    }
                }
                Note::Failed(error) => panic!("{error}"),
            }
        }
        panic!("no hint for {path}; seen {seen:?}");
    }

    #[test]
    fn create_replace_by_rename_and_delete_are_hinted_and_temporary_files_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("bindings")).unwrap();
        let (bus, received) = listen(root);

        let started = Instant::now();
        fs::write(root.join("a.json"), "{}").unwrap();
        let seen = hinted(&received, "a.json");
        assert!(started.elapsed() < Duration::from_millis(500), "hinted after {:?}", started.elapsed());
        assert!(seen.iter().all(|path| no_temporary(path)));

        // Replace by rename: the hint names the replaced file, never the temporary one.
        fs::write(root.join(".hgs-next"), "[]").unwrap();
        fs::rename(root.join(".hgs-next"), root.join("a.json")).unwrap();
        let seen = hinted(&received, "a.json");
        assert!(!seen.contains(Path::new(".hgs-next")), "{seen:?}");

        // Subfolders are watched; names are relative to the root.
        fs::write(root.join("bindings").join("b.json"), "{}").unwrap();
        hinted(&received, r"bindings\b.json");

        fs::remove_file(root.join("a.json")).unwrap();
        hinted(&received, "a.json");

        // Only ignored names: nothing is delivered.
        fs::write(root.join(".hgs-other"), "x").unwrap();
        assert!(received.recv_timeout(Duration::from_millis(300)).is_err(), "a temporary file wakes nobody");

        drop(bus);
        fs::write(root.join("c.json"), "{}").unwrap();
        assert!(received.recv_timeout(Duration::from_millis(300)).is_err(), "a stopped watcher sends nothing");
    }

    #[test]
    fn a_burst_is_delivered_once() {
        let dir = tempfile::tempdir().unwrap();
        let (_bus, received) = listen(dir.path());
        for index in 0..20 {
            fs::write(dir.path().join(format!("{index}.json")), "{}").unwrap();
        }
        let mut seen = BTreeSet::new();
        let mut hints = 0;
        while seen.len() < 20 {
            match received.recv_timeout(Duration::from_secs(2)) {
                Ok(Note::Hint(hint)) => {
                    hints += 1;
                    seen.extend(hint.paths);
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(hints <= 2, "20 writes in a burst came as {hints} hints");
    }

    /// 10 000 files while the receiver does not read: the system's buffer
    /// overflows and the watcher says so instead of naming a partial set.
    #[test]
    fn an_overflow_is_a_hint_that_names_everything() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // A rendezvous: the watcher waits in `sink` until the test reads.
        let (notes, received) = mpsc::sync_channel::<Note>(0);
        let bus = Bus::start(root, no_temporary, move |note| notes.send(note).is_ok()).unwrap();
        for index in 0..10_000 {
            fs::write(root.join(format!("file-with-a-longer-name-{index:05}.json")), "").unwrap();
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
        assert!(overflow, "10 000 files overflow the buffer");
        // The watcher goes on after an overflow.
        while received.try_recv().is_ok() {}
        std::thread::sleep(Duration::from_millis(200));
        while received.try_recv().is_ok() {}
        fs::write(root.join("after.json"), "{}").unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        let mut seen = false;
        while let Ok(note) = received.recv_timeout(until.saturating_duration_since(Instant::now())) {
            if let Note::Hint(hint) = note {
                if hint.overflow || hint.paths.contains(Path::new("after.json")) {
                    seen = true;
                    break;
                }
            }
        }
        assert!(seen, "a change after the overflow is hinted");
        // The receiver goes first: a watcher waiting in `sink` then stops.
        drop(received);
        drop(bus);
    }

    #[test]
    fn a_missing_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let error = Bus::start(&missing, no_temporary, |_| true).err().unwrap();
        assert!(error.contains("cannot watch"), "{error}");
    }

    #[test]
    fn a_damaged_record_is_an_overflow() {
        let mut batch = Batch::default();
        // NextEntryOffset 0, Action 1, FileNameLength 100: longer than the bytes.
        let mut bytes = vec![0u8; 16];
        bytes[4] = 1;
        bytes[8] = 100;
        parse(&bytes, no_temporary, &mut batch);
        assert_eq!(batch.take(), Some(Hint { paths: BTreeSet::new(), overflow: true }));
        // Two good records.
        let mut bytes = Vec::new();
        for (name, last) in [("a.json", false), ("b.json", true)] {
            let name: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let size = (12 + name.len()).div_ceil(4) * 4;
            let next = if last { 0 } else { size as u32 };
            bytes.extend_from_slice(&next.to_le_bytes());
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.extend_from_slice(&(name.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&name);
            bytes.resize(bytes.len().div_ceil(4) * 4, 0);
        }
        parse(&bytes, no_temporary, &mut batch);
        let hint = batch.take().unwrap();
        assert_eq!(hint.paths, ["a.json", "b.json"].map(PathBuf::from).into_iter().collect());
    }
}
