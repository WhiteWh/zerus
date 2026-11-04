//! Activity attachments read into the private cache, and opened (Qt's
//! `AttachmentLoader`, `AttachmentViewer.h:162-243`): one read per file
//! however many views ask, at most three reads at once with an Open ahead of
//! thumbnails, and the last files read answered from their copies while the
//! copies last. Thumbnails show a copy through `zerus-file`; playback reads
//! a bounded raw IPC copy because WebKit's media backend refuses that scheme.
//!
//! Opening follows Qt's `ExternalFile`: an Activity attachment is another
//! machine's file (marked as downloaded, `AttachmentLoader::open`), a
//! composer's attachment this computer's (`MessageComposer::openAttachment`),
//! and a type that runs programs is shown in the file manager instead.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use zerus_client_core::attachments;
use zerus_client_core::external_file::{self, Origin, Outcome};
use zerus_client_core::reads::{AttachmentRef, CachedAttachment};
use zerus_client_core::ComputerId;

use super::files::Cache;
use super::{lock, Composers, Spawn};

/// Reads one attachment into the cache root it is given (`hgs attachment`).
pub type FetchFn = Arc<
    dyn Fn(&ComputerId, &AttachmentRef, &Path) -> Result<CachedAttachment, String> + Send + Sync,
>;

/// A system call on a file: open it with its default application, or show
/// it in the file manager. True when it started.
pub type PathFn = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

/// The system's open and reveal.
#[derive(Clone)]
pub struct Opener {
    pub open: PathFn,
    pub reveal: PathFn,
}

/// An attachment in the cache, as a view shows it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Fetched {
    /// `cache/{digest}/{name}`, the copy's `zerus-file` path.
    pub path: String,
    pub name: String,
    pub mime: String,
    pub bytes: u64,
    /// An image a card may show a thumbnail of: its header gives its size,
    /// at most the shared thumbnail pixel bound (`AttachmentLoader::thumbnail`).
    pub thumbnail: bool,
    pub image_size: Option<attachments::image::Size>,
}

/// Reads at once (`AttachmentLoader::pump`).
const RUNNING: usize = 3;
/// Files read whose copies answer again without a read (`m_paths`).
const REMEMBERED: usize = 256;
/// Qt draws no thumbnail of a larger image (`AttachmentLoader::thumbnail`).
/// How much of a copy the size check reads: a JPEG's frame header can
/// follow long metadata.
const HEADER_BYTES: u64 = attachments::image::HEADER_BYTES as u64;

/// `AttachmentLoader::fetch` for a file its message saved no reference to.
pub const NO_REFERENCE: &str = "This attachment has no saved file reference.";
/// `AttachmentLoader::open` when no application opened the file.
pub const OPEN_FAILED: &str = "No application could open this file.";
/// `MessageComposer::openAttachment` when the copy could not be made.
pub const DRAFT_UNAVAILABLE: &str = "Could not open the attachment.";
/// `MessageComposer::openAttachment` when no application opened the file.
pub const DRAFT_OPEN_FAILED: &str = "Could not open the attachment with its default application.";

type Answer = Result<(Fetched, PathBuf), String>;

/// Gets one read's answer, once: no thread waits for it.
type Waiter = Box<dyn FnOnce(Answer) + Send>;

struct Job {
    computer: ComputerId,
    file: AttachmentRef,
    waiters: Vec<Waiter>,
}

#[derive(Default)]
struct Queue {
    jobs: HashMap<String, Job>,
    /// Jobs not started yet, the next first.
    waiting: VecDeque<String>,
    running: usize,
    /// Files read, oldest first, with their copies.
    read: HashMap<String, (Fetched, PathBuf)>,
    order: VecDeque<String>,
}

impl Queue {
    /// The jobs to start now, keeping at most `RUNNING` reads.
    fn next(&mut self) -> Vec<(String, ComputerId, AttachmentRef)> {
        let mut start = Vec::new();
        while self.running < RUNNING {
            let Some(key) = self.waiting.pop_front() else {
                break;
            };
            if let Some(job) = self.jobs.get(&key) {
                self.running += 1;
                start.push((key, job.computer.clone(), job.file.clone()));
            }
        }
        start
    }

    fn remember(&mut self, key: String, found: (Fetched, PathBuf)) {
        if self.read.insert(key.clone(), found).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > REMEMBERED {
            if let Some(oldest) = self.order.pop_front() {
                self.read.remove(&oldest);
            }
        }
    }
}

/// Reads Activity attachments into the cache.
pub struct Fetcher {
    cache: Arc<Cache>,
    fetch: FetchFn,
    spawn: Spawn,
    queue: Mutex<Queue>,
}

/// One file of one message of one conversation on one computer
/// (`AttachmentLoader::fetch`'s key).
fn key_of(computer: &str, file: &AttachmentRef) -> String {
    serde_json::json!([
        computer,
        file.name,
        file.conversation_id,
        file.archive_id,
        file.agent_id,
        file.request_id,
        file.index
    ])
    .to_string()
}

impl Fetcher {
    pub fn new(cache: Arc<Cache>, fetch: FetchFn, spawn: Spawn) -> Arc<Self> {
        Arc::new(Fetcher {
            cache,
            fetch,
            spawn,
            queue: Mutex::new(Queue::default()),
        })
    }

    pub fn cache(&self) -> &Arc<Cache> {
        &self.cache
    }

    /// The file in the cache. A caller for a file being read joins that
    /// read; `priority` moves it ahead of the reads waiting. Nothing waits on
    /// a thread: only the reads, at most `RUNNING`, use the blocking pool.
    pub async fn fetch(
        self: &Arc<Self>,
        computer: &str,
        file: AttachmentRef,
        priority: bool,
    ) -> Result<Fetched, String> {
        self.local(computer, file, priority)
            .await
            .map(|(fetched, _)| fetched)
    }

    async fn local(
        self: &Arc<Self>,
        computer: &str,
        file: AttachmentRef,
        priority: bool,
    ) -> Answer {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.request(
            computer,
            file,
            priority,
            Box::new(move |answer| {
                let _ = sender.send(answer);
            }),
        );
        receiver
            .await
            .map_err(|_| "Could not read the attachment.".to_owned())?
    }

    /// Asks for the file; `done` gets the answer once: at once for a file
    /// without a reference or a copy read before, else from the read.
    fn request(
        self: &Arc<Self>,
        computer: &str,
        file: AttachmentRef,
        priority: bool,
        done: Waiter,
    ) {
        if file.request_id.is_empty() {
            return done(Err(NO_REFERENCE.to_owned()));
        }
        let key = key_of(computer, &file);
        // A file read before answers from its copy while the copy lasts.
        let known = lock(&self.queue).read.get(&key).cloned();
        if let Some((fetched, path)) = known {
            if attachments::cached_at(self.cache.root(), &path).is_some() {
                attachments::touch(&path);
                return done(Ok((fetched, path)));
            }
        }
        let start = {
            let mut queue = lock(&self.queue);
            if let Some(job) = queue.jobs.get_mut(&key) {
                job.waiters.push(done);
                let waiting = queue.waiting.iter().position(|k| *k == key);
                if let (true, Some(at)) = (priority, waiting) {
                    queue.waiting.remove(at);
                    queue.waiting.push_front(key);
                }
            } else {
                queue.jobs.insert(
                    key.clone(),
                    Job {
                        computer: ComputerId(computer.to_owned()),
                        file,
                        waiters: vec![done],
                    },
                );
                if priority {
                    queue.waiting.push_front(key);
                } else {
                    queue.waiting.push_back(key);
                }
            }
            queue.next()
        };
        self.start(start);
    }

    fn start(self: &Arc<Self>, jobs: Vec<(String, ComputerId, AttachmentRef)>) {
        for (key, computer, file) in jobs {
            let me = Arc::clone(self);
            (self.spawn)(Box::new(move || me.run(key, computer, file)));
        }
    }

    fn run(self: &Arc<Self>, key: String, computer: ComputerId, file: AttachmentRef) {
        // A read that panics anywhere (the read, its description or the
        // prune it starts) still ends: its slot frees and its waiters hear.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let result = (self.fetch)(&computer, &file, self.cache.root())
                .and_then(|cached| self.described(cached));
            if result.is_ok() {
                self.cache.prune_later(&self.spawn, Arc::new(HashSet::new));
            }
            result
        }))
        .unwrap_or_else(|_| Err("Could not read the attachment.".to_owned()));
        let (waiters, next) = {
            let mut queue = lock(&self.queue);
            queue.running -= 1;
            let waiters = queue
                .jobs
                .remove(&key)
                .map(|job| job.waiters)
                .unwrap_or_default();
            if let Ok(found) = &result {
                queue.remember(key, found.clone());
            }
            (waiters, queue.next())
        };
        for waiter in waiters {
            waiter(result.clone());
        }
        self.start(next);
    }

    /// The view's description of a copy the read stored.
    pub(crate) fn described(&self, cached: CachedAttachment) -> Answer {
        let path = attachments::cached_at(self.cache.root(), &cached.local_path)
            .ok_or_else(|| "Could not save the attachment.".to_owned())?;
        let relative = path
            .strip_prefix(self.cache.root())
            .map_err(|_| "Could not save the attachment.".to_owned())?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
        let mut head = Vec::new();
        if let Ok(copy) = std::fs::File::open(&path) {
            let _ = copy.take(HEADER_BYTES).read_to_end(&mut head);
        }
        let mime = match cached.metadata.get("mime").and_then(|v| v.as_str()) {
            Some(mime) if !mime.is_empty() => mime.to_owned(),
            _ => attachments::sniff(&head[..head.len().min(512)]),
        };
        let image_size = attachments::image::dimensions(&head);
        let thumbnail = image_size.is_some();
        let name = match cached.metadata.get("name").and_then(|v| v.as_str()) {
            Some(name) if !name.is_empty() => name.to_owned(),
            _ => "attachment".to_owned(),
        };
        Ok((
            Fetched {
                path: format!("cache/{relative}"),
                name,
                mime,
                bytes,
                thumbnail,
                image_size,
            },
            path,
        ))
    }

    /// Opens an Activity attachment (`AttachmentLoader::open`): a read ahead
    /// of the others, then the copy as another machine's file, on the
    /// blocking pool.
    pub async fn open(
        self: &Arc<Self>,
        computer: &str,
        file: AttachmentRef,
        opener: Opener,
    ) -> Result<Option<String>, String> {
        let (_, path) = self.local(computer, file, true).await?;
        tauri::async_runtime::spawn_blocking(move || {
            opened(&path, Origin::OtherMachine, &opener, OPEN_FAILED)
        })
        .await
        .map_err(|error| error.to_string())?
    }
}

/// Opens or reveals a copy and says what happened. The path loses
/// Windows's `\\?\` prefix first, which canonical paths carry and which
/// neither a notice nor an opener should see.
pub(crate) fn opened(
    path: &Path,
    origin: Origin,
    opener: &Opener,
    failed: &str,
) -> Result<Option<String>, String> {
    let path = dunce::simplified(path);
    let outcome = external_file::open(path, origin, |p| (opener.open)(p), |p| (opener.reveal)(p));
    told(outcome, path, failed)
}

#[cfg(test)]
fn image_size(head: &[u8]) -> Option<(u32, u32)> {
    attachments::image::dimensions(head).map(|size| (size.width, size.height))
}

/// What an open tells the person: nothing when it opened, a notice when the
/// file manager shows a program type, otherwise why it did not open.
fn told(outcome: Outcome, path: &Path, failed: &str) -> Result<Option<String>, String> {
    match outcome {
        Outcome::Opened => Ok(None),
        Outcome::Revealed => Ok(Some(external_file::revealed_notice(path))),
        Outcome::RevealFailed => Err(external_file::reveal_failed_notice(path)),
        Outcome::OpenFailed => Err(failed.to_owned()),
    }
}

/// Opens a composer's attachment (`MessageComposer::openAttachment`) by
/// the view's `draft/{handle}/{list}/{index}`: its copy in the cache, as
/// this computer's file. Nothing happens while the draft sends, or when its
/// attachment list is no longer the one the view showed.
pub fn open_draft(
    composers: &Composers,
    path: &str,
    opener: &Opener,
) -> Result<Option<String>, String> {
    let numbers = path
        .strip_prefix("draft/")
        .and_then(super::files::draft_numbers);
    let attachment = numbers.and_then(|[handle, list, index]| {
        let hub = composers.lock();
        hub.composers
            .values()
            .find(|c| c.handle == handle && c.attachments_rev == list && !c.sending)
            .and_then(|c| {
                c.draft
                    .attachments
                    .get(usize::try_from(index).ok()?)
                    .cloned()
            })
    });
    let Some(attachment) = attachment else {
        return Ok(None);
    };
    let cache = composers.cache();
    let files = attachments::prepare(cache.root(), std::slice::from_ref(&attachment))?;
    let path = files
        .into_iter()
        .next()
        .map(|file| file.local_path)
        .ok_or_else(|| DRAFT_UNAVAILABLE.to_owned())?;
    cache.prune_later(&composers.0.deps.spawn, Arc::new(HashSet::new));
    opened(&path, Origin::ThisComputer, opener, DRAFT_OPEN_FAILED)
}

/// Reads an Activity attachment into the cache for a view, after the reads
/// ahead of it.
#[tauri::command]
pub async fn attachment_fetch(
    fetcher: tauri::State<'_, Arc<Fetcher>>,
    computer: String,
    file: AttachmentRef,
) -> Result<Fetched, String> {
    // Awaited, not waited for: a queued read holds no thread.
    let fetcher = Arc::clone(fetcher.inner());
    fetcher.fetch(&computer, file, false).await
}

/// Read a recording through the same receipt-bound, bounded attachment
/// queue. The browser receives raw bytes, never a local or remote file path.
#[tauri::command]
pub async fn attachment_media(
    fetcher: tauri::State<'_, Arc<Fetcher>>,
    computer: String,
    file: AttachmentRef,
    video: bool,
) -> Result<tauri::ipc::Response, String> {
    let fetcher = Arc::clone(fetcher.inner());
    let fetched = fetcher.fetch(&computer, file, true).await?;
    let cache = Arc::clone(fetcher.cache());
    tauri::async_runtime::spawn_blocking(move || {
        cache
            .recording(&fetched.path, video)
            .map(tauri::ipc::Response::new)
    })
    .await
    .map_err(|error| error.to_string())?
}

/// A view shows the copy at `path` (`cache/{digest}/{name}`).
#[tauri::command]
pub fn attachment_pin(fetcher: tauri::State<'_, Arc<Fetcher>>, path: String) {
    if let Some(copy) = fetcher.cache().shown(&path) {
        fetcher.cache().pin(&copy);
    }
}

/// A view no longer shows the copy at `path`.
#[tauri::command]
pub fn attachment_unpin(fetcher: tauri::State<'_, Arc<Fetcher>>, path: String) {
    if let Some(copy) = fetcher.cache().shown_at(&path) {
        fetcher.cache().unpin(&copy);
    }
}

/// Opens an Activity attachment: `Ok(None)` when it opened, `Ok(notice)`
/// when the file manager shows it instead, otherwise why not.
#[tauri::command]
pub async fn attachment_open(
    fetcher: tauri::State<'_, Arc<Fetcher>>,
    opener: tauri::State<'_, Opener>,
    computer: String,
    file: AttachmentRef,
) -> Result<Option<String>, String> {
    let (fetcher, opener) = (Arc::clone(fetcher.inner()), opener.inner().clone());
    fetcher.open(&computer, file, opener).await
}

/// Opens the composer attachment the view lists at `path`
/// (`draft/{handle}/{list}/{index}`), as `attachment_open` answers.
#[tauri::command]
pub async fn attachment_open_draft(
    composers: tauri::State<'_, Composers>,
    opener: tauri::State<'_, Opener>,
    path: String,
) -> Result<Option<String>, String> {
    let (composers, opener) = (composers.inner().clone(), opener.inner().clone());
    tauri::async_runtime::spawn_blocking(move || open_draft(&composers, &path, &opener))
        .await
        .map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer::tests::{dir, hub, session, until};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Condvar;
    use std::time::{Duration, Instant};
    use zerus_client_core::attachments::Selection;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";

    fn file(request: &str) -> AttachmentRef {
        AttachmentRef {
            name: "claude/sample-project/review".into(),
            request_id: request.into(),
            index: 0,
            conversation_id: "conv-1".into(),
            archive_id: None,
            agent_id: None,
        }
    }

    /// `fetch`, waited for on a test thread.
    fn get(
        f: &Arc<Fetcher>,
        computer: &str,
        file: AttachmentRef,
        priority: bool,
    ) -> Result<Fetched, String> {
        tauri::async_runtime::block_on(f.fetch(computer, file, priority))
    }

    /// `open`, waited for on a test thread.
    fn open_on(
        f: &Arc<Fetcher>,
        computer: &str,
        file: AttachmentRef,
        opener: &Opener,
    ) -> Result<Option<String>, String> {
        tauri::async_runtime::block_on(f.open(computer, file, opener.clone()))
    }

    fn threads() -> Spawn {
        Arc::new(|job| {
            std::thread::spawn(job);
        })
    }

    /// A read that stores `name` with `bytes`, counting its calls.
    fn storing(name: &'static str, bytes: &'static [u8], calls: &Arc<AtomicUsize>) -> FetchFn {
        let calls = Arc::clone(calls);
        Arc::new(move |_, file, root| {
            calls.fetch_add(1, Ordering::SeqCst);
            let local_path = attachments::store_at(root, name, bytes).unwrap();
            Ok(CachedAttachment {
                metadata: serde_json::json!({"name": name, "request_id": file.request_id}),
                local_path,
            })
        })
    }

    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn the_same_attachment_is_read_once_for_two_views() {
        let root = dir("fetch-once");
        let calls = Arc::new(AtomicUsize::new(0));
        let slow = storing("shot.png", PNG, &calls);
        let fetch: FetchFn = Arc::new(move |computer, file, root| {
            std::thread::sleep(Duration::from_millis(100));
            slow(computer, file, root)
        });
        let fetcher = Fetcher::new(Cache::new(root.join("cache")), fetch, threads());
        let (a, b) = (Arc::clone(&fetcher), Arc::clone(&fetcher));
        let one = std::thread::spawn(move || get(&a, "studio", file("req-1"), false));
        let two = std::thread::spawn(move || get(&b, "studio", file("req-1"), true));
        let (one, two) = (one.join().unwrap().unwrap(), two.join().unwrap().unwrap());
        assert_eq!(one, two);
        assert!(one.path.starts_with("cache/") && one.path.ends_with("/shot.png"));
        assert_eq!(
            (one.name.as_str(), one.mime.as_str(), one.bytes),
            ("shot.png", "image/png", PNG.len() as u64)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            fetcher.cache().shown(&one.path).is_some(),
            "the path names the copy"
        );
    }

    #[test]
    fn three_reads_run_at_once_and_an_open_goes_next() {
        let root = dir("fetch-queue");
        // Each read waits for a permit; `started` lists the reads in order.
        let gate = Arc::new((Mutex::new(0usize), Condvar::new()));
        let started = Arc::new(Mutex::new(Vec::<String>::new()));
        let (running, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let fetch: FetchFn = {
            let (gate, started, running, most) = (
                Arc::clone(&gate),
                Arc::clone(&started),
                Arc::clone(&running),
                Arc::clone(&most),
            );
            Arc::new(move |_, file, root| {
                started.lock().unwrap().push(file.request_id.clone());
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                let (permits, ready) = &*gate;
                let mut permits = ready
                    .wait_while(permits.lock().unwrap(), |n| *n == 0)
                    .unwrap();
                *permits -= 1;
                drop(permits);
                running.fetch_sub(1, Ordering::SeqCst);
                let local_path = attachments::store_at(
                    root,
                    &format!("{}.txt", file.request_id),
                    file.request_id.as_bytes(),
                )
                .unwrap();
                Ok(CachedAttachment {
                    metadata: serde_json::json!({}),
                    local_path,
                })
            })
        };
        let fetcher = Fetcher::new(Cache::new(root.join("cache")), fetch, threads());
        let ask = |request: &str, priority: bool| {
            let (me, request) = (Arc::clone(&fetcher), request.to_owned());
            std::thread::spawn(move || get(&me, "studio", file(&request), priority))
        };
        let mut asked: Vec<_> = (1..=5).map(|n| ask(&format!("req-{n}"), false)).collect();
        wait_for("three reads and two waiting", || {
            started.lock().unwrap().len() == 3 && lock(&fetcher.queue).waiting.len() == 2
        });
        asked.push(ask("req-6", true));
        wait_for("the open to queue", || {
            lock(&fetcher.queue).waiting.len() == 3
        });
        let release = |n: usize| {
            *gate.0.lock().unwrap() += n;
            gate.1.notify_all();
        };
        release(1);
        wait_for("a fourth read", || started.lock().unwrap().len() == 4);
        assert_eq!(
            started.lock().unwrap()[3],
            "req-6",
            "the open goes ahead of the waiting reads"
        );
        release(10);
        for answer in asked {
            assert!(answer.join().unwrap().is_ok());
        }
        assert_eq!(
            most.load(Ordering::SeqCst),
            3,
            "never more than three reads"
        );
        assert_eq!(started.lock().unwrap().len(), 6);
        let queue = lock(&fetcher.queue);
        assert!(queue.jobs.is_empty() && queue.waiting.is_empty() && queue.running == 0);
    }

    #[test]
    fn a_file_read_before_answers_from_its_copy_while_it_lasts() {
        let root = dir("fetch-known");
        let calls = Arc::new(AtomicUsize::new(0));
        let fetcher = Fetcher::new(
            Cache::new(root.join("cache")),
            storing("notes.txt", b"plain words", &calls),
            threads(),
        );
        let first = get(&fetcher, "studio", file("req-1"), false).unwrap();
        assert_eq!(
            first.mime, "text/plain",
            "sniffed when the reply names no type"
        );
        assert_eq!(
            get(&fetcher, "studio", file("req-1"), false).unwrap(),
            first
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "answered from the copy");
        // Another computer's file of the same name is another file.
        get(&fetcher, "mini", file("req-1"), false).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        std::fs::remove_file(fetcher.cache().shown(&first.path).unwrap()).unwrap();
        assert_eq!(
            get(&fetcher, "studio", file("req-1"), false).unwrap(),
            first
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "a vanished copy is read again"
        );
    }

    #[test]
    fn the_files_remembered_stay_bounded() {
        let root = dir("fetch-bound");
        let calls = Arc::new(AtomicUsize::new(0));
        let fetcher = Fetcher::new(
            Cache::new(root.join("cache")),
            storing("a.txt", b"a", &calls),
            Arc::new(|job| job()),
        );
        for n in 0..REMEMBERED + 4 {
            get(&fetcher, "studio", file(&format!("req-{n}")), false).unwrap();
        }
        let queue = lock(&fetcher.queue);
        assert_eq!(
            (queue.read.len(), queue.order.len()),
            (REMEMBERED, REMEMBERED)
        );
        assert!(
            !queue.read.contains_key(&key_of("studio", &file("req-0"))),
            "the oldest went first"
        );
    }

    #[test]
    fn a_file_without_a_reference_is_never_read() {
        let root = dir("fetch-unsaved");
        let calls = Arc::new(AtomicUsize::new(0));
        let fetcher = Fetcher::new(
            Cache::new(root.join("cache")),
            storing("a.txt", b"a", &calls),
            threads(),
        );
        assert_eq!(
            get(&fetcher, "studio", file(""), false),
            Err(NO_REFERENCE.to_owned())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_failed_read_tells_every_view_and_is_read_again_next_time() {
        let root = dir("fetch-failed");
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let fetch: FetchFn = Arc::new(move |_, _, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(50));
            Err("Attachment response is too large.".to_owned())
        });
        let fetcher = Fetcher::new(Cache::new(root.join("cache")), fetch, threads());
        let other = Arc::clone(&fetcher);
        let second = std::thread::spawn(move || get(&other, "studio", file("req-1"), false));
        let first = get(&fetcher, "studio", file("req-1"), false);
        assert_eq!(first, Err("Attachment response is too large.".to_owned()));
        assert_eq!(second.join().unwrap(), first);
        let _ = get(&fetcher, "studio", file("req-1"), false);
        assert!(
            calls.load(Ordering::SeqCst) >= 2,
            "nothing failed is remembered"
        );
    }

    /// What an opener was asked: "open" or "reveal", and the file.
    type Asked = Arc<Mutex<Vec<(String, PathBuf)>>>;

    /// An opener that records what it was asked and answers `works`.
    fn opener(works: bool) -> (Opener, Asked) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (opened, revealed) = (Arc::clone(&asked), Arc::clone(&asked));
        (
            Opener {
                open: Arc::new(move |path| {
                    opened
                        .lock()
                        .unwrap()
                        .push(("open".into(), path.to_path_buf()));
                    works
                }),
                reveal: Arc::new(move |path| {
                    revealed
                        .lock()
                        .unwrap()
                        .push(("reveal".into(), path.to_path_buf()));
                    works
                }),
            },
            asked,
        )
    }

    #[test]
    fn an_activity_attachment_opens_as_another_machines_file() {
        let root = dir("open-activity");
        let calls = Arc::new(AtomicUsize::new(0));
        let fetcher = Fetcher::new(
            Cache::new(root.join("cache")),
            storing("shot.png", PNG, &calls),
            threads(),
        );
        let (works, asked) = opener(true);
        assert_eq!(open_on(&fetcher, "studio", file("req-1"), &works), Ok(None));
        let (what, path) = asked.lock().unwrap()[0].clone();
        assert_eq!(what, "open");
        assert!(path.starts_with(fetcher.cache().root()));
        assert_eq!(std::fs::read(&path).unwrap(), PNG);
        let (fails, _) = opener(false);
        assert_eq!(
            open_on(&fetcher, "studio", file("req-1"), &fails),
            Err(OPEN_FAILED.to_owned())
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "Open and the thumbnail share the read"
        );
        assert_eq!(
            open_on(&fetcher, "studio", file(""), &works),
            Err(NO_REFERENCE.to_owned())
        );
    }

    #[test]
    fn a_program_is_shown_in_the_file_manager_instead() {
        let root = dir("open-program");
        let calls = Arc::new(AtomicUsize::new(0));
        let fetcher = Fetcher::new(
            Cache::new(root.join("cache")),
            storing("setup.exe", b"MZ program", &calls),
            threads(),
        );
        let (works, asked) = opener(true);
        let shown = open_on(&fetcher, "studio", file("req-1"), &works)
            .unwrap()
            .unwrap();
        assert_eq!(shown, "setup.exe can run programs, so it is shown in the file manager instead of being opened.");
        assert_eq!(asked.lock().unwrap()[0].0, "reveal");
        let (fails, _) = opener(false);
        let error = open_on(&fetcher, "studio", file("req-1"), &fails).unwrap_err();
        assert!(error.starts_with("setup.exe can run programs, and the file manager could not show it. Its copy is at "), "{error}");
    }

    #[test]
    fn a_composer_attachment_opens_its_copy_while_the_list_is_the_one_shown() {
        let root = dir("open-draft");
        let original = root.join("notes.txt");
        std::fs::write(&original, b"draft notes").unwrap();
        let (c, _) = hub(&root);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.attach_paths(view.handle, vec![original.clone()], Selection::default());
        let view = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        let path = view.attachments[0].path.clone();
        let (works, asked) = opener(true);
        assert_eq!(open_draft(&c, &path, &works), Ok(None));
        let (what, copy) = asked.lock().unwrap()[0].clone();
        assert_eq!(what, "open");
        assert!(
            copy.starts_with(c.cache().root()),
            "the cache's copy, never the original"
        );
        assert_eq!(std::fs::read(&copy).unwrap(), b"draft notes");
        let (fails, _) = opener(false);
        assert_eq!(
            open_draft(&c, &path, &fails),
            Err(DRAFT_OPEN_FAILED.to_owned())
        );
        // A list that changed opens nothing, even where its old index now
        // holds another file; nor does an address that names no attachment.
        let other = root.join("other.txt");
        std::fs::write(&other, b"other notes").unwrap();
        c.attach_paths(view.handle, vec![other], Selection::default());
        until(&c, |v| v.attachments.len() == 2 && !v.attaching);
        c.remove(view.handle, 0);
        let moved = until(&c, |v| v.attachments.len() == 1);
        assert_eq!(
            moved.attachments[0].name, "other.txt",
            "index 0 now holds another file"
        );
        let (quiet, untouched) = opener(true);
        for gone in [path.as_str(), "draft/x/1/0", "cache/abc/notes.txt", ""] {
            assert_eq!(open_draft(&c, gone, &quiet), Ok(None), "{gone}");
        }
        assert!(untouched.lock().unwrap().is_empty());
    }

    /// A read that stores `name` with `bytes` and names `mime` in its reply.
    fn storing_typed(name: &'static str, bytes: &'static [u8], mime: &'static str) -> FetchFn {
        Arc::new(move |_, file, root| {
            let local_path = attachments::store_at(root, name, bytes).unwrap();
            Ok(CachedAttachment {
                metadata: serde_json::json!({"name": name, "mime": mime, "request_id": file.request_id}),
                local_path,
            })
        })
    }

    #[test]
    fn queued_reads_hold_no_thread() {
        let root = dir("fetch-threads");
        // The jobs the Fetcher hands the blocking pool, held here unrun.
        let jobs: Arc<Mutex<Vec<crate::composer::Job>>> = Arc::default();
        let queue = Arc::clone(&jobs);
        let spawn: Spawn = Arc::new(move |job| queue.lock().unwrap().push(job));
        let calls = Arc::new(AtomicUsize::new(0));
        let fetcher = Fetcher::new(
            Cache::new(root.join("cache")),
            storing("a.txt", b"a", &calls),
            spawn,
        );
        let answered = Arc::new(AtomicUsize::new(0));
        for n in 0..40 {
            let answered = Arc::clone(&answered);
            fetcher.request(
                "studio",
                file(&format!("req-{n}")),
                false,
                Box::new(move |answer| {
                    assert!(answer.is_ok());
                    answered.fetch_add(1, Ordering::SeqCst);
                }),
            );
        }
        assert_eq!(
            jobs.lock().unwrap().len(),
            RUNNING,
            "only the reads take the pool; 37 waiters hold nothing"
        );
        assert_eq!(answered.load(Ordering::SeqCst), 0);
        // Each read that ends starts the next one (prunes share the pool too).
        loop {
            let next = jobs.lock().unwrap().pop();
            let Some(job) = next else { break };
            job();
            assert!(lock(&fetcher.queue).running <= RUNNING);
        }
        assert_eq!(answered.load(Ordering::SeqCst), 40);
        assert_eq!(calls.load(Ordering::SeqCst), 40);
    }

    #[test]
    fn a_read_that_panics_frees_its_slot_and_tells_its_views() {
        let root = dir("fetch-panic");
        let fetch: FetchFn = Arc::new(|_, file, root| {
            if file.request_id == "req-bad" {
                panic!("a broken read");
            }
            let local_path = attachments::store_at(root, "a.txt", b"a").unwrap();
            Ok(CachedAttachment {
                metadata: serde_json::json!({}),
                local_path,
            })
        });
        let fetcher = Fetcher::new(Cache::new(root.join("cache")), fetch, Arc::new(|job| job()));
        assert_eq!(
            get(&fetcher, "studio", file("req-bad"), false),
            Err("Could not read the attachment.".to_owned())
        );
        assert_eq!(lock(&fetcher.queue).running, 0);
        assert!(
            get(&fetcher, "studio", file("req-good"), false).is_ok(),
            "the queue still runs"
        );
    }

    #[test]
    fn a_waiting_read_asked_again_with_priority_goes_next() {
        let root = dir("fetch-requeue");
        let jobs: Arc<Mutex<Vec<crate::composer::Job>>> = Arc::default();
        let queue = Arc::clone(&jobs);
        let spawn: Spawn = Arc::new(move |job| queue.lock().unwrap().push(job));
        let calls = Arc::new(AtomicUsize::new(0));
        let fetcher = Fetcher::new(
            Cache::new(root.join("cache")),
            storing("a.txt", b"a", &calls),
            spawn,
        );
        for n in 1..=5 {
            fetcher.request("studio", file(&format!("req-{n}")), false, Box::new(|_| {}));
        }
        assert_eq!(
            lock(&fetcher.queue).waiting,
            [
                key_of("studio", &file("req-4")),
                key_of("studio", &file("req-5"))
            ]
        );
        fetcher.request("studio", file("req-5"), true, Box::new(|_| {}));
        assert_eq!(
            lock(&fetcher.queue).waiting,
            [
                key_of("studio", &file("req-5")),
                key_of("studio", &file("req-4"))
            ]
        );
        assert_eq!(
            lock(&fetcher.queue).jobs[&key_of("studio", &file("req-5"))]
                .waiters
                .len(),
            2,
            "one read for both"
        );
    }

    #[test]
    fn the_replys_type_wins_over_the_sniffed_one() {
        let root = dir("fetch-mime");
        let fetcher = Fetcher::new(
            Cache::new(root.join("cache")),
            storing_typed("notes.md", b"plain words", "text/markdown"),
            threads(),
        );
        let read = get(&fetcher, "studio", file("req-1"), false).unwrap();
        assert_eq!(
            (read.mime.as_str(), read.thumbnail),
            ("text/markdown", false)
        );
    }

    #[test]
    fn image_headers_give_their_size() {
        let png = |w: u32, h: u32| {
            [
                b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".as_slice(),
                &w.to_be_bytes(),
                &h.to_be_bytes(),
            ]
            .concat()
        };
        assert_eq!(image_size(&png(160, 100)), Some((160, 100)));
        assert_eq!(image_size(b"GIF89a\xa0\x00\x64\x00rest"), Some((160, 100)));
        let mut bmp = vec![0u8; 26];
        bmp[..2].copy_from_slice(b"BM");
        bmp[18..22].copy_from_slice(&160i32.to_le_bytes());
        bmp[22..26].copy_from_slice(&(-100i32).to_le_bytes());
        assert_eq!(image_size(&bmp), Some((160, 100)));
        let webp = |kind: &[u8], body: &[u8]| [b"RIFF\0\0\0\0WEBP".as_slice(), kind, body].concat();
        let mut lossy = vec![0u8; 14];
        lossy[10..12].copy_from_slice(&160u16.to_le_bytes());
        lossy[12..14].copy_from_slice(&100u16.to_le_bytes());
        assert_eq!(image_size(&webp(b"VP8 ", &lossy)), Some((160, 100)));
        let bits: u32 = (160 - 1) | ((100 - 1) << 14);
        let lossless = [&[0u8; 5][..], &bits.to_le_bytes()].concat();
        assert_eq!(image_size(&webp(b"VP8L", &lossless)), Some((160, 100)));
        let extended = [&[0u8; 8][..], &[159, 0, 0], &[99, 0, 0]].concat();
        assert_eq!(image_size(&webp(b"VP8X", &extended)), Some((160, 100)));
        // A JPEG's frame header after an EXIF segment.
        let jpeg = [
            &[0xff, 0xd8, 0xff, 0xe1, 0x00, 0x06][..],
            b"Exif",
            &[0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x64, 0x00, 0xa0, 0x03],
        ]
        .concat();
        assert_eq!(image_size(&jpeg), Some((160, 100)));
        assert_eq!(
            image_size(&[0xff, 0xd8, 0xff, 0xda, 0x00, 0x02]),
            None,
            "no frame before the scan"
        );
        assert_eq!(
            image_size(b"<svg xmlns='http://www.w3.org/2000/svg'/>"),
            None
        );
        assert_eq!(image_size(b"plain words"), None);
        assert_eq!(
            image_size(&png(160, 100)[..20]),
            None,
            "a cut header gives nothing"
        );
    }

    #[test]
    fn only_an_image_within_qts_bound_gets_a_thumbnail() {
        let root = dir("fetch-thumbnail");
        let fetcher = Fetcher::new(
            Cache::new(root.join("cache")),
            storing("shot.png", PNG, &Arc::new(AtomicUsize::new(0))),
            threads(),
        );
        // PNG is a cut header ("rest"): no size, so no thumbnail, as Qt's reader.
        assert!(
            !get(&fetcher, "studio", file("req-1"), false)
                .unwrap()
                .thumbnail
        );
        const SMALL: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\xa0\0\0\0\x64";
        const HUGE: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\x23\x28\0\0\x23\x28";
        let small = Fetcher::new(
            Cache::new(root.join("small")),
            storing("small.png", SMALL, &Arc::new(AtomicUsize::new(0))),
            threads(),
        );
        assert!(
            get(&small, "studio", file("req-1"), false)
                .unwrap()
                .thumbnail
        );
        let huge = Fetcher::new(
            Cache::new(root.join("huge")),
            storing("huge.png", HUGE, &Arc::new(AtomicUsize::new(0))),
            threads(),
        );
        assert!(
            !get(&huge, "studio", file("req-1"), false)
                .unwrap()
                .thumbnail,
            "9000x9000 is past 64M pixels"
        );
    }

    #[test]
    fn only_another_machines_file_is_marked_as_downloaded() {
        let root = dir("open-marker");
        let fetcher = Fetcher::new(
            Cache::new(root.join("cache")),
            storing("notes.txt", b"notes", &Arc::new(AtomicUsize::new(0))),
            threads(),
        );
        let (works, asked) = opener(true);
        open_on(&fetcher, "studio", file("req-1"), &works).unwrap();
        let activity = asked.lock().unwrap()[0].1.clone();
        // Linux has no downloaded-file marker; macOS and Windows carry one.
        let marks = cfg!(any(target_os = "macos", windows));
        assert_eq!(!external_file::download_marker(&activity).is_empty(), marks);
        let original = root.join("draft.txt");
        std::fs::write(&original, b"draft").unwrap();
        let (c, _) = hub(&root);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.attach_paths(view.handle, vec![original], Selection::default());
        let view = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        let (draft_opener, draft_asked) = opener(true);
        open_draft(&c, &view.attachments[0].path, &draft_opener).unwrap();
        let draft = draft_asked.lock().unwrap()[0].1.clone();
        assert!(
            external_file::download_marker(&draft).is_empty(),
            "this computer's file is never marked"
        );
    }

    #[test]
    fn a_draft_that_sends_opens_nothing() {
        let root = dir("open-sending");
        let original = root.join("notes.txt");
        std::fs::write(&original, b"draft notes").unwrap();
        let (c, _) = hub(&root);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.attach_paths(view.handle, vec![original], Selection::default());
        let view = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        let path = view.attachments[0].path.clone();
        for composer in c.lock().composers.values_mut() {
            composer.sending = true;
        }
        let (works, asked) = opener(true);
        assert_eq!(open_draft(&c, &path, &works), Ok(None));
        assert!(asked.lock().unwrap().is_empty());
    }

    #[test]
    fn a_zero_by_zero_header_gets_no_thumbnail() {
        let root = dir("fetch-empty-image");
        const EMPTY: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\0\0\0\0\0";
        let fetcher = Fetcher::new(
            Cache::new(root.join("cache")),
            storing("empty.png", EMPTY, &Arc::new(AtomicUsize::new(0))),
            threads(),
        );
        assert!(
            !get(&fetcher, "studio", file("req-1"), false)
                .unwrap()
                .thumbnail
        );
    }

    #[test]
    fn a_panic_after_the_read_ends_it_too() {
        let root = dir("fetch-panic-prune");
        // The pool runs the read, then refuses the prune the read starts.
        let spawned = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&spawned);
        let spawn: Spawn = Arc::new(move |job| {
            if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                job();
            } else {
                panic!("the pool refused the prune");
            }
        });
        let fetcher = Fetcher::new(
            Cache::new(root.join("cache")),
            storing("a.txt", b"a", &Arc::new(AtomicUsize::new(0))),
            spawn,
        );
        let answer = Arc::new(Mutex::new(None));
        let heard = Arc::clone(&answer);
        fetcher.request(
            "studio",
            file("req-1"),
            false,
            Box::new(move |a| {
                *heard.lock().unwrap() = Some(a.map(|(f, _)| f.name));
            }),
        );
        assert_eq!(
            *answer.lock().unwrap(),
            Some(Err("Could not read the attachment.".to_owned()))
        );
        assert_eq!(lock(&fetcher.queue).running, 0, "the slot is free");
        assert!(lock(&fetcher.queue).jobs.is_empty());
    }
}
