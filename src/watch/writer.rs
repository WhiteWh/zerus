//! The only owner of the stream's stdout: a bounded queue of encoded bodies,
//! frame numbering at write time, and heartbeats.
use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use zerus_watch_protocol as protocol;

/// One queued frame body (`{"t":...}`, see `protocol::encode_body`).
pub(super) struct Item {
    /// Bodies with the same key replace each other while queued: the latest
    /// `state` of a key, or the latest `catalog.meta`.
    pub coalesce: Option<String>,
    pub body: String,
}

struct Queue {
    items: VecDeque<Item>,
    bytes: usize,
}

struct Shared {
    queue: Mutex<Queue>,
    /// Signals the writer thread that a body is queued.
    ready: Condvar,
    /// Signals producers that the queue has room.
    space: Condvar,
    capacity: usize,
    stopped: AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Clone)]
pub(super) struct Writer {
    shared: Arc<Shared>,
}

impl Writer {
    /// Starts the thread that writes to `out`. `closed` runs once when `out`
    /// fails, as when the client closed the pipe.
    pub(super) fn spawn<W, F>(out: W, capacity: usize, heartbeat: Duration, closed: F) -> Self
    where
        W: Write + Send + 'static,
        F: FnOnce() + Send + 'static,
    {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue { items: VecDeque::new(), bytes: 0 }),
            ready: Condvar::new(),
            space: Condvar::new(),
            capacity,
            stopped: AtomicBool::new(false),
        });
        let thread = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("hgs-watch-writer".into())
            .spawn(move || {
                write_frames(&thread, out, heartbeat);
                thread.stopped.store(true, Ordering::SeqCst);
                thread.space.notify_all();
                closed();
            })
            .expect("start the watch writer thread");
        Self { shared }
    }

    /// Queues a body unless it would overflow the queue; an empty queue always
    /// takes one. A body whose `coalesce` key is already queued replaces that
    /// body in place. False means the client fell behind: the caller owes the
    /// topic a `gap` and a fresh snapshot.
    pub(super) fn offer(&self, item: Item) -> bool {
        if self.stopped() {
            return false;
        }
        let mut queue = self.shared.lock();
        if let Some(key) = item.coalesce.as_deref() {
            if let Some(queued) = queue.items.iter_mut().find(|queued| queued.coalesce.as_deref() == Some(key)) {
                let previous = std::mem::replace(&mut queued.body, item.body);
                let current = queued.body.len();
                queue.bytes = queue.bytes - previous.len() + current;
                return true;
            }
        }
        if !queue.items.is_empty() && queue.bytes + item.body.len() > self.shared.capacity {
            return false;
        }
        queue.bytes += item.body.len();
        queue.items.push_back(item);
        self.shared.ready.notify_one();
        true
    }

    /// Queues a body, waiting while the queue is full. False once the stream
    /// has stopped.
    pub(super) fn put(&self, item: Item) -> bool {
        let mut queue = self.shared.lock();
        while !queue.items.is_empty() && queue.bytes + item.body.len() > self.shared.capacity {
            if self.stopped() {
                return false;
            }
            queue = self
                .shared
                .space
                .wait_timeout(queue, Duration::from_millis(100))
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
        if self.stopped() {
            return false;
        }
        queue.bytes += item.body.len();
        queue.items.push_back(item);
        self.shared.ready.notify_one();
        true
    }

    /// Bytes queued and not yet written.
    pub(super) fn backlog(&self) -> usize {
        self.shared.lock().bytes
    }

    /// Stops the stream: queued frames are dropped and every wait ends.
    pub(super) fn halt(&self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        let _queue = self.shared.lock();
        self.shared.space.notify_all();
        self.shared.ready.notify_all();
    }

    pub(super) fn stopped(&self) -> bool {
        self.shared.stopped.load(Ordering::SeqCst)
    }

    /// Waits up to `limit` for the queue to empty; for tests and orderly ends.
    #[cfg(test)]
    pub(super) fn drain(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            if self.backlog() == 0 || self.stopped() {
                return self.backlog() == 0;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn write_frames<W: Write>(shared: &Shared, mut out: W, heartbeat: Duration) {
    let mut seq = 0u64;
    let mut last = Instant::now();
    loop {
        let body = {
            let mut queue = shared.lock();
            loop {
                if let Some(item) = queue.items.pop_front() {
                    queue.bytes -= item.body.len();
                    shared.space.notify_all();
                    break item.body;
                }
                let idle = last.elapsed();
                if idle >= heartbeat {
                    let beat = protocol::Body::Heartbeat(protocol::Heartbeat { at: now() });
                    break protocol::encode_body(&beat).expect("a heartbeat encodes");
                }
                queue = shared
                    .ready
                    .wait_timeout(queue, heartbeat - idle)
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .0;
            }
        };
        seq += 1;
        let Ok(mut line) = protocol::envelope(seq, &body) else {
            continue;
        };
        line.push('\n');
        if out.write_all(line.as_bytes()).and_then(|()| out.flush()).is_err() {
            return;
        }
        last = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::mpsc;

    /// A pipe end the test reads, optionally blocked until released.
    #[derive(Clone)]
    struct Sink {
        data: Arc<Mutex<Vec<u8>>>,
        gate: Arc<(Mutex<bool>, Condvar)>,
        fail: Arc<AtomicBool>,
    }
    impl Sink {
        fn new(open: bool) -> Self {
            Self {
                data: Arc::default(),
                gate: Arc::new((Mutex::new(open), Condvar::new())),
                fail: Arc::default(),
            }
        }
        fn open(&self) {
            *self.gate.0.lock().unwrap() = true;
            self.gate.1.notify_all();
        }
        fn lines(&self) -> Vec<serde_json::Value> {
            String::from_utf8(self.data.lock().unwrap().clone())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }
    }
    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut open = self.gate.0.lock().unwrap();
            while !*open {
                open = self.gate.1.wait(open).unwrap();
            }
            if self.fail.load(Ordering::SeqCst) {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }
            self.data.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn body(text: &str) -> String {
        protocol::encode_body(&protocol::Body::Pong(protocol::Pong { id: Some(text.into()) })).unwrap()
    }

    fn item(text: &str, coalesce: Option<&str>) -> Item {
        Item { coalesce: coalesce.map(str::to_owned), body: body(text) }
    }

    #[test]
    fn observing_the_pong_before_subscribe_prevents_cross_fence_coalescing() {
        let sink = Sink::new(false);
        let writer = Writer::spawn(sink.clone(), 1 << 20, Duration::from_secs(60), || {});
        assert!(writer.put(item("blocked", None)));
        std::thread::sleep(Duration::from_millis(20));
        assert!(writer.offer(item("old", Some("state|processes|run:r"))));
        assert!(writer.put(item("fence", None)));
        sink.open();
        let until = Instant::now() + Duration::from_secs(5);
        while !sink.lines().iter().any(|line| line["id"] == "fence") {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(2));
        }
        // Only now may the new subscription enqueue/coalesce its first state.
        assert!(writer.offer(item("new", Some("state|processes|run:r"))));
        while sink.lines().len() < 4 {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(2));
        }
        let ids: Vec<_> = sink.lines().iter().map(|line| line["id"].as_str().unwrap().to_owned()).collect();
        assert_eq!(ids, ["blocked", "old", "fence", "new"]);
        writer.halt();
    }

    #[test]
    fn frames_are_numbered_in_write_order_and_coalesce_while_queued() {
        let sink = Sink::new(false);
        let writer = Writer::spawn(sink.clone(), 1 << 20, Duration::from_secs(60), || {});
        assert!(writer.offer(item("first", None)));
        // The writer thread may already hold "first"; queue behind it.
        std::thread::sleep(Duration::from_millis(20));
        assert!(writer.offer(item("state-1", Some("state|activity|run:a"))));
        assert!(writer.offer(item("events", None)));
        assert!(writer.offer(item("state-2", Some("state|activity|run:a"))));
        assert!(writer.offer(item("other", Some("state|activity|run:b"))));
        sink.open();
        assert!(writer.drain(Duration::from_secs(5)));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while sink.lines().len() < 4 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let lines = sink.lines();
        let ids: Vec<_> = lines.iter().map(|line| line["id"].as_str().unwrap().to_owned()).collect();
        assert_eq!(ids, ["first", "state-2", "events", "other"]);
        let seqs: Vec<_> = lines.iter().map(|line| line["seq"].as_u64().unwrap()).collect();
        assert_eq!(seqs, [1, 2, 3, 4]);
        assert!(lines.iter().all(|line| line["v"] == 1 && line["t"] == "pong"));
    }

    #[test]
    fn a_full_queue_refuses_offers_and_put_waits_for_room() {
        let sink = Sink::new(false);
        let one = body("x").len();
        let writer = Writer::spawn(sink.clone(), 3 * one, Duration::from_secs(60), || {});
        assert!(writer.offer(item("x", None)));
        std::thread::sleep(Duration::from_millis(20));
        let mut accepted = 0;
        while writer.offer(item("x", None)) {
            accepted += 1;
            assert!(accepted <= 4, "the queue never filled");
        }
        assert!(writer.backlog() <= 3 * one);
        let (done, waited) = mpsc::channel();
        let blocked = writer.clone();
        std::thread::spawn(move || done.send(blocked.put(item("y", None))).unwrap());
        assert!(waited.recv_timeout(Duration::from_millis(100)).is_err(), "put returned while full");
        sink.open();
        assert_eq!(waited.recv_timeout(Duration::from_secs(5)), Ok(true));
        assert!(writer.drain(Duration::from_secs(5)));
    }

    #[test]
    fn heartbeats_follow_silence_and_a_closed_pipe_stops_the_writer() {
        let sink = Sink::new(true);
        let (closed, was_closed) = mpsc::channel();
        let writer = Writer::spawn(sink.clone(), 1 << 20, Duration::from_millis(50), move || closed.send(()).unwrap());
        // A loaded runner can delay the writer thread; wait for two heartbeats
        // rather than for a fixed time.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while sink.lines().len() < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let lines = sink.lines();
        assert!(lines.len() >= 2, "{lines:?}");
        assert!(lines.iter().all(|line| line["t"] == "heartbeat" && line["at"].as_f64().unwrap() > 1e9));
        sink.fail.store(true, Ordering::SeqCst);
        writer.offer(item("lost", None));
        was_closed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(writer.stopped());
        assert!(!writer.offer(item("after", None)));
        assert!(!writer.put(item("after", None)));
    }
}
