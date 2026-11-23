//! One session of the session host (D6, D7, D9, D13): its pane's screen model,
//! the threads that pump its pseudoconsole and the clients attached to it.
//!
//! A session owns three threads. The reader feeds everything the console writes
//! to the screen model, sends the model's answers to queries back as input (the
//! host is the only responder, D6) and relays the output without its queries to
//! attached clients. The writer is the only thread that writes the console's
//! input, which can block; keys, pastes, query answers and client input reach
//! it through a channel, client input after its terminal's own answers are
//! removed. Sent Ctrl+letter keys reach a program that reads key records as
//! win32-input-mode records once the console asks for that mode (D6). The
//! host's waiter thread waits for the root process (the inbox pseudoconsole's
//! output does not end before it is closed, D6), then closes the console off
//! the reader thread, lets the output drain and ends the session's job.
//!
//! Everything platform-specific (the pseudoconsole, the job and the process)
//! sits behind `Launcher` and `Console`, so this file and its tests are plain
//! Rust; `console.rs` implements them with ConPTY on Windows.
use super::{
    channel,
    keys::{self, Modes},
    protocol::{ConsoleEvidence, Control, CreateSession, ErrorBody, ErrorCode, Key, Size},
    sequences::{QueryStripper, ReplyFilter},
    spawn::{Environment, EnvironmentError},
};
use crate::screen::{Capture, Info, ScreenModel};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    io::{self, Read, Write},
    sync::{
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
        Arc, Condvar, Mutex, MutexGuard, PoisonError,
    },
    thread,
    time::{Duration, Instant},
};

/// How long a client's incomplete escape sequence waits for the rest before it
/// counts as typed keys (a lone ESC).
pub(crate) const ESCAPE_TIME: Duration = Duration::from_millis(50);
/// A kill waits this long for the root process after the console closed
/// (`CTRL_CLOSE_EVENT` gives processes 5 s), then ends the job.
pub(crate) const KILL_GRACE: Duration = Duration::from_secs(6);
/// After the root process exits, the output may take this long to end.
const DRAIN: Duration = Duration::from_secs(5);
/// Output queued for one client beyond this is replaced by a repaint.
const OUTBOX_LIMIT: usize = 4 << 20;
/// WhiteWh fork: the lines of history a client gets when it attaches: all
/// the screen model keeps (`screen::HISTORY_LIMIT`).
const ATTACH_HISTORY: usize = 2000;
const READ_BUFFER: usize = 64 * 1024;
/// The bracketed-paste markers tmux writes around a paste.
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What starting a session needs, beyond its IDs.
pub(crate) struct LaunchPlan<'a> {
    /// The root process's command line; its program is an absolute path.
    pub argv: &'a [String],
    /// Its absolute working directory.
    pub cwd: &'a str,
    pub environment: &'a Environment,
    pub size: Size,
}

/// A started session: the root process and its console.
pub(crate) struct Launched {
    pub pid: u32,
    /// What the console renders.
    pub output: Box<dyn Read + Send>,
    /// The console's input.
    pub input: Box<dyn Write + Send>,
    pub console: Box<dyn Console>,
}

/// Starts a session's root process on a console of its own.
pub(crate) trait Launcher: Send + Sync + 'static {
    fn launch(&self, plan: &LaunchPlan<'_>) -> io::Result<Launched>;
}

/// A session's console, root process and job.
pub(crate) trait Console: Send + Sync {
    fn resize(&self, size: Size) -> io::Result<()>;
    /// Wait until the root process has exited; its exit code when known.
    fn wait(&self) -> Option<u32>;
    /// Whether the root process has exited within `timeout`.
    fn wait_exit(&self, timeout: Duration) -> bool;
    /// Close the console without waiting: every process attached to it gets
    /// `CTRL_CLOSE_EVENT`. Later calls do nothing.
    fn hang_up(&self);
    /// End every process left in the session's job, after a closing console
    /// has finished or a bounded wait; returns within a bounded time.
    fn end(&self);
    /// The program in the foreground of the pane, as a file stem.
    fn current_command(&self) -> String;
    /// Whether `pid` is the root process or runs inside the session's job.
    fn holds(&self, pid: u32) -> bool;
}

/// The environment of a session's root process (D8): the creating client's
/// whole block, the terminal type, and the session's IDs, which only the host
/// knows. `COLORTERM` is removed: agents choose colours from `TERM` alone.
pub(crate) fn environment(
    block: &[String],
    session: &str,
    pane: &str,
) -> Result<Environment, EnvironmentError> {
    let mut environment = Environment::parse(block)?;
    environment.set("TERM", "xterm-256color")?;
    environment.remove("COLORTERM")?;
    environment.set("HGS_HOST_SESSION", session)?;
    environment.set("HGS_HOST_PANE", pane)?;
    Ok(environment)
}

/// Input for a session's console.
enum Input {
    /// From the host: keys, pastes and query answers.
    Host(Vec<u8>),
    /// What an attached client typed, before its terminal's answers are removed.
    Client(u64, Vec<u8>),
    /// A client left; an escape sequence it began goes out as typed.
    Gone(u64),
}

/// What the host sends an attached client.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Out {
    Stream(Vec<u8>),
    Control(Control),
}

#[derive(Default)]
struct Queue {
    items: VecDeque<Out>,
    bytes: usize,
    closed: bool,
}

/// One attached client (D13).
pub(crate) struct Attachment {
    pub id: u64,
    pub pid: u32,
    size: Mutex<Size>,
    queue: Mutex<Queue>,
    ready: Condvar,
}

impl Attachment {
    pub(crate) fn new(id: u64, pid: u32, size: Size) -> Arc<Self> {
        Arc::new(Self {
            id,
            pid,
            size: Mutex::new(size),
            queue: Mutex::new(Queue::default()),
            ready: Condvar::new(),
        })
    }

    /// The next thing to send; None once closed and drained.
    pub(crate) fn next(&self) -> Option<Out> {
        let mut queue = lock(&self.queue);
        loop {
            if let Some(item) = queue.items.pop_front() {
                if let Out::Stream(bytes) = &item {
                    queue.bytes -= bytes.len();
                }
                return Some(item);
            }
            if queue.closed {
                return None;
            }
            queue = self
                .ready
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Queue output. A client that has fallen this far behind gets a repaint
    /// in place of everything it has not read, as tmux redraws a slow client.
    fn push_stream(&self, bytes: &[u8], repaint: impl FnOnce() -> Vec<u8>) {
        let mut queue = lock(&self.queue);
        if queue.closed {
            return;
        }
        if queue.bytes + bytes.len() > OUTBOX_LIMIT {
            queue.items.retain(|item| matches!(item, Out::Control(_)));
            let repaint = repaint();
            queue.bytes = repaint.len();
            queue.items.push_back(Out::Stream(repaint));
        } else {
            queue.bytes += bytes.len();
            queue.items.push_back(Out::Stream(bytes.to_vec()));
        }
        self.ready.notify_all();
    }

    fn push_control(&self, control: Control) {
        let mut queue = lock(&self.queue);
        if !queue.closed {
            queue.items.push_back(Out::Control(control));
            self.ready.notify_all();
        }
    }

    /// Nothing more is queued; `next` returns what is left, then None.
    pub(crate) fn close(&self) {
        lock(&self.queue).closed = true;
        self.ready.notify_all();
    }
}

struct Meta {
    name: String,
    /// Session variables, the run marker (`@hgs_run`) included.
    variables: BTreeMap<String, String>,
    /// The root process has exited.
    dead: bool,
    /// A kill or the end has begun.
    stopping: bool,
}

struct Screen {
    model: ScreenModel,
    stripper: QueryStripper,
    /// Attached clients, the most recently active last: the pane takes its
    /// size (tmux `window-size latest`, D13).
    clients: Vec<Arc<Attachment>>,
    size: Size,
    /// The session has ended; nothing attaches any more.
    ended: bool,
}

/// A running session and its one pane.
pub(crate) struct Session {
    /// The number in `$N`.
    pub id: u64,
    /// The number in `%N`.
    pub pane: u64,
    pub launch_id: String,
    /// The absolute directory the root process started in.
    pub cwd: String,
    /// When it started, in Unix seconds.
    pub created: u64,
    /// The root process (the `--run` supervisor).
    pub pid: u32,
    console: Box<dyn Console>,
    meta: Mutex<Meta>,
    screen: Mutex<Screen>,
    input: Mutex<Option<Sender<Input>>>,
    drained: (Mutex<bool>, Condvar),
    /// Task #14 (P2.4): raised by every output the console writes and when
    /// the root process exits; `list` reports it per pane.
    output_count: std::sync::atomic::AtomicU64,
}

fn exited(pane: &str) -> ErrorBody {
    ErrorBody::new(ErrorCode::NotFound, format!("pane {pane} has exited"))
}

impl Session {
    /// Take over the session `request` launched and start its reader and
    /// writer threads.
    pub(crate) fn start(
        id: u64,
        pane: u64,
        request: &CreateSession,
        created: u64,
        launched: Launched,
    ) -> io::Result<Arc<Self>> {
        let Launched {
            pid,
            output,
            input,
            console,
        } = launched;
        let size = request.size;
        let (sender, inbox) = mpsc::channel();
        let session = Arc::new(Self {
            id,
            pane,
            launch_id: request.launch_id.clone(),
            cwd: request.cwd.clone(),
            created,
            pid,
            console,
            meta: Mutex::new(Meta {
                name: request.name.clone(),
                variables: BTreeMap::new(),
                dead: false,
                stopping: false,
            }),
            screen: Mutex::new(Screen {
                model: ScreenModel::new(size.rows, size.cols),
                stripper: QueryStripper::default(),
                clients: Vec::new(),
                size,
                ended: false,
            }),
            input: Mutex::new(Some(sender)),
            drained: (Mutex::new(false), Condvar::new()),
            output_count: std::sync::atomic::AtomicU64::new(0),
        });
        let started = thread::Builder::new()
            .name(format!("hgs-pane-{pane}-input"))
            .spawn(move || write_input(input, inbox))
            .and_then(|_| {
                let reader = session.clone();
                thread::Builder::new()
                    .name(format!("hgs-pane-{pane}-output"))
                    .spawn(move || read_output(&reader, output))
            });
        if let Err(error) = started {
            session.console.hang_up();
            session.console.end();
            return Err(error);
        }
        Ok(session)
    }

    pub(crate) fn session_id(&self) -> String {
        format!("${}", self.id)
    }

    pub(crate) fn pane_id(&self) -> String {
        format!("%{}", self.pane)
    }

    pub(crate) fn name(&self) -> String {
        lock(&self.meta).name.clone()
    }

    pub(crate) fn set_name(&self, name: &str) {
        lock(&self.meta).name = name.to_owned();
    }

    pub(crate) fn variable(&self, name: &str) -> Option<String> {
        lock(&self.meta).variables.get(name).cloned()
    }

    pub(crate) fn set_variable(&self, name: &str, value: Option<&str>) {
        let mut meta = lock(&self.meta);
        match value {
            Some(value) => meta.variables.insert(name.to_owned(), value.to_owned()),
            None => meta.variables.remove(name),
        };
    }

    pub(crate) fn dead(&self) -> bool {
        lock(&self.meta).dead
    }

    /// How often the pane changed: its output, and its root process's exit.
    pub(crate) fn output_count(&self) -> u64 {
        self.output_count.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn info(&self) -> Info {
        lock(&self.screen).model.info()
    }

    pub(crate) fn capture(&self, styled: bool) -> String {
        let mode = if styled {
            Capture::Styled
        } else {
            Capture::Plain
        };
        lock(&self.screen).model.capture(mode)
    }

    /// The visible screen as text, wrapped rows joined (`ScreenModel::joined_text`).
    pub(crate) fn joined_text(&self) -> String {
        lock(&self.screen).model.joined_text()
    }

    pub(crate) fn current_command(&self) -> String {
        self.console.current_command()
    }

    /// Whether `pid` is the root process or one of its descendants in the job.
    pub(crate) fn holds(&self, pid: u32) -> bool {
        pid == self.pid || self.console.holds(pid)
    }

    /// Whether the root process exits within `timeout`.
    pub(crate) fn wait_exit(&self, timeout: Duration) -> bool {
        self.console.wait_exit(timeout)
    }

    /// The attached clients' IDs and process IDs.
    pub(crate) fn clients(&self) -> Vec<(u64, u32)> {
        lock(&self.screen)
            .clients
            .iter()
            .map(|client| (client.id, client.pid))
            .collect()
    }

    fn send(&self, input: Input) {
        if let Some(sender) = lock(&self.input).as_ref() {
            let _ = sender.send(input);
        }
    }

    /// Write `keys` as `send-keys` does, in the pane's current modes. When a
    /// key is Ctrl and a letter and the console wants win32-input-mode, the
    /// supervisor's `evidence` (None without one or its answer), asked once,
    /// tells whether the program reads key records (`channel::key_records`).
    pub(crate) fn send_keys(
        &self,
        keys: &[Key],
        evidence: impl FnOnce() -> Option<ConsoleEvidence>,
    ) -> Result<(), ErrorBody> {
        if self.dead() {
            return Err(exited(&self.pane_id()));
        }
        let (mut modes, win32_input) = {
            let screen = lock(&self.screen);
            let info = screen.model.info();
            let modes = Modes {
                cursor: info.application_cursor_keys,
                keypad: info.application_keypad,
                key_records: false,
            };
            (modes, screen.stripper.win32_input())
        };
        let ctrl_letter = keys
            .iter()
            .any(|key| matches!(key, Key::Named(name) if keys::ctrl_letter(name, modes)));
        if ctrl_letter {
            modes.key_records = channel::key_records(win32_input, evidence);
        }
        let bytes: Vec<u8> = keys
            .iter()
            .flat_map(|key| match key {
                Key::Named(name) => keys::named(name, modes),
                Key::Literal(text) => keys::literal(text),
            })
            .collect();
        if !bytes.is_empty() {
            self.send(Input::Host(bytes));
        }
        Ok(())
    }

    /// Write `text` as `paste-buffer -r` does: line feeds kept, control
    /// characters shown as tmux shows them (`paste_bytes`), inside
    /// bracketed-paste markers when `bracketed` (`-p`) and the application
    /// enabled them.
    pub(crate) fn paste(&self, text: &str, bracketed: bool) -> Result<(), ErrorBody> {
        if self.dead() {
            return Err(exited(&self.pane_id()));
        }
        self.send(Input::Host(paste_bytes(
            text,
            bracketed && self.info().bracketed_paste,
        )));
        Ok(())
    }

    /// Resize the pane to the most recently active client's size.
    fn follow_latest(&self, screen: &mut Screen) {
        let Some(latest) = screen.clients.last() else {
            return;
        };
        let size = *lock(&latest.size);
        if size == screen.size || size.cols == 0 || size.rows == 0 {
            return;
        }
        if self.console.resize(size).is_ok() {
            screen.model.resize(size.rows, size.cols);
            screen.size = size;
        }
    }

    /// Make the pane `rows` tall, keeping its width, while no client is
    /// attached; the next client to attach sets its size again. False when
    /// the size stays as it was.
    pub(crate) fn grow_unviewed(&self, rows: u16) -> Result<bool, ErrorBody> {
        let mut screen = lock(&self.screen);
        if screen.ended || self.dead() {
            return Err(exited(&self.pane_id()));
        }
        if !screen.clients.is_empty() || rows <= screen.size.rows {
            return Ok(false);
        }
        let size = Size {
            cols: screen.size.cols,
            rows,
        };
        if self.console.resize(size).is_err() {
            return Ok(false);
        }
        screen.model.resize(size.rows, size.cols);
        screen.size = size;
        Ok(true)
    }

    /// Attach a client: the pane takes its size, and its first output is a
    /// repaint of the screen, after which it gets everything the console writes.
    pub(crate) fn attach(&self, client: Arc<Attachment>) -> Result<(), ErrorBody> {
        let mut screen = lock(&self.screen);
        if screen.ended || self.dead() {
            return Err(exited(&self.pane_id()));
        }
        screen.clients.push(client.clone());
        self.follow_latest(&mut screen);
        // WhiteWh fork: the pane's history first, so the client's terminal has
        // scrollback above the screen it attaches to.
        let mut repaint = screen.model.history(ATTACH_HISTORY);
        repaint.extend(screen.model.repaint());
        // A repaint past the queue's limit still goes out whole.
        client.push_stream(&repaint, || repaint.clone());
        Ok(())
    }

    /// The client leaves; the session runs on.
    pub(crate) fn detach(&self, id: u64) {
        let mut screen = lock(&self.screen);
        if let Some(index) = screen.clients.iter().position(|client| client.id == id) {
            screen.clients.remove(index).close();
            self.follow_latest(&mut screen);
        }
        drop(screen);
        self.send(Input::Gone(id));
    }

    /// Mark client `id` as the most recently active one.
    fn activity(&self, screen: &mut Screen, id: u64) -> bool {
        let Some(index) = screen.clients.iter().position(|client| client.id == id) else {
            return false;
        };
        let client = screen.clients.remove(index);
        screen.clients.push(client);
        self.follow_latest(screen);
        true
    }

    /// Input typed in client `id`.
    pub(crate) fn client_input(&self, id: u64, bytes: Vec<u8>) {
        if self.activity(&mut lock(&self.screen), id) {
            self.send(Input::Client(id, bytes));
        }
    }

    /// Client `id`'s terminal has a new size.
    pub(crate) fn client_resize(&self, id: u64, size: Size) {
        let mut screen = lock(&self.screen);
        if let Some(client) = screen.clients.iter().find(|client| client.id == id) {
            *lock(&client.size) = size;
        }
        self.activity(&mut screen, id);
    }

    /// Show `text` on client `id`; whether it is attached here.
    pub(crate) fn notify(&self, id: u64, text: &str) -> bool {
        let screen = lock(&self.screen);
        let Some(client) = screen.clients.iter().find(|client| client.id == id) else {
            return false;
        };
        client.push_control(Control::Notice { text: text.into() });
        true
    }

    /// Everything the console wrote: into the model, answers back to the
    /// console, and the rest without its queries to every attached client.
    fn output(&self, bytes: &[u8]) {
        let mut screen = lock(&self.screen);
        let Screen {
            model,
            stripper,
            clients,
            ..
        } = &mut *screen;
        model.process(bytes);
        self.output_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let replies = model.take_replies();
        if !replies.is_empty() {
            self.send(Input::Host(replies));
        }
        let relayed = stripper.strip(bytes);
        if !relayed.is_empty() {
            for client in clients.iter() {
                client.push_stream(&relayed, || model.repaint());
            }
        }
    }

    fn output_ended(&self) {
        *lock(&self.drained.0) = true;
        self.drained.1.notify_all();
    }

    /// End the session as tmux `kill-session` hangs up a pane: close the console
    /// now, and end the job if the root process has not exited after
    /// `KILL_GRACE`. Returns at once; the waiter finishes the session.
    pub(crate) fn kill(self: &Arc<Self>) {
        {
            let mut meta = lock(&self.meta);
            if meta.stopping {
                return;
            }
            meta.stopping = true;
        }
        self.console.hang_up();
        let session = self.clone();
        let ender = move || {
            if !session.console.wait_exit(KILL_GRACE) {
                session.console.end();
            }
        };
        let spawned = thread::Builder::new()
            .name(format!("hgs-pane-{}-kill", self.pane))
            .spawn(ender);
        if spawned.is_err() {
            // Without a thread the job ends now, without the grace period.
            self.console.end();
        }
    }

    /// Wait for the root process to exit, then close the console, let the
    /// output drain and end the job (D6). Its exit code when known. Runs on the
    /// host's waiter thread.
    pub(crate) fn watch(&self) -> Option<i32> {
        let code = self.console.wait().map(|code| code as i32);
        {
            let mut meta = lock(&self.meta);
            meta.dead = true;
            meta.stopping = true;
        }
        self.output_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.console.hang_up();
        let deadline = Instant::now() + DRAIN;
        let mut drained = lock(&self.drained.0);
        while !*drained {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            drained = self
                .drained
                .1
                .wait_timeout(drained, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        drop(drained);
        self.console.end();
        code
    }

    /// The session has ended: attached clients get its exit, and its input
    /// writer stops.
    pub(crate) fn finish(&self, code: Option<i32>) {
        let mut screen = lock(&self.screen);
        screen.ended = true;
        for client in screen.clients.drain(..) {
            client.push_control(Control::Exit { code });
            client.close();
        }
        drop(screen);
        lock(&self.input).take();
    }
}

/// The pane's foreground process, as `pane_current_command` names it: Windows
/// has no foreground process group, so it is the newest of the root process's
/// children in the session job (`(pid, parent, creation time)`), as an agent
/// the supervisor started or a command a shell runs, else the root itself.
pub(crate) fn foreground(root: u32, processes: &[(u32, u32, u64)]) -> u32 {
    processes
        .iter()
        .filter(|(_, parent, _)| *parent == root)
        .max_by_key(|(pid, _, created)| (*created, *pid))
        .map_or(root, |(pid, _, _)| *pid)
}

/// The bytes of a paste as tmux 3.7 and later write them (`paste-buffer -r`,
/// with `-p` when `bracket`), inside the markers when `bracket`. tmux passes
/// the text through vis(3) with `VIS_SAFE | VIS_NOSLASH` unless `-S` is
/// given, which hgs never does: an ASCII control character other than tab,
/// line feed, carriage return, backspace and bell is written as `^` and a
/// letter (ESC as `^[`, DEL as `^?`), so a paste can neither end bracketed
/// paste early nor carry escape sequences. Every other character, UTF-8 C1
/// controls included, is written unchanged (checked against tmux 3.8 for every
/// Unicode scalar value).
pub(crate) fn paste_bytes(text: &str, bracket: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() + 12);
    if bracket {
        bytes.extend_from_slice(PASTE_START);
    }
    for byte in text.bytes() {
        match byte {
            0x7f => bytes.extend_from_slice(b"^?"),
            b'\t' | b'\n' | b'\r' | 0x08 | 0x07 => bytes.push(byte),
            0x00..=0x1f => bytes.extend_from_slice(&[b'^', byte + b'@']),
            _ => bytes.push(byte),
        }
    }
    if bracket {
        bytes.extend_from_slice(PASTE_END);
    }
    bytes
}

fn read_output(session: &Session, mut output: Box<dyn Read + Send>) {
    let mut buffer = vec![0; READ_BUFFER];
    loop {
        match output.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => session.output(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    session.output_ended();
}

/// The only writer of a console's input. Each client's input passes its own
/// `ReplyFilter`; a sequence a client began and did not finish within
/// `ESCAPE_TIME` goes out as typed.
fn write_input(mut input: Box<dyn Write + Send>, inbox: Receiver<Input>) {
    let mut filters: HashMap<u64, (ReplyFilter, Instant)> = HashMap::new();
    loop {
        let deadline = filters
            .values()
            .filter(|(filter, _)| filter.holding())
            .map(|(_, last)| *last + ESCAPE_TIME)
            .min();
        let next = match deadline {
            Some(deadline) => {
                match inbox.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(item) => Some(item),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            None => match inbox.recv() {
                Ok(item) => Some(item),
                Err(_) => break,
            },
        };
        let now = Instant::now();
        let bytes = match next {
            None => filters
                .values_mut()
                .filter(|(filter, last)| filter.holding() && now >= *last + ESCAPE_TIME)
                .flat_map(|(filter, _)| filter.flush())
                .collect(),
            Some(Input::Host(bytes)) => bytes,
            Some(Input::Client(id, bytes)) => {
                let (filter, last) = filters
                    .entry(id)
                    .or_insert_with(|| (ReplyFilter::default(), now));
                *last = now;
                filter.filter(&bytes)
            }
            Some(Input::Gone(id)) => filters
                .remove(&id)
                .map(|(mut filter, _)| filter.flush())
                .unwrap_or_default(),
        };
        if !bytes.is_empty()
            && input
                .write_all(&bytes)
                .and_then(|()| input.flush())
                .is_err()
        {
            break;
        }
    }
}
