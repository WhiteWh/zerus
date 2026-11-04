//! The embedded Terminal's attach clients, ported from `tray/src/TerminalView.cpp`:
//! one `hgs [@host] a NAME --existing [--run-id RUN]` per view, in a
//! pseudoterminal (`zerus_client_core::terminal::Attach`). Output goes to the
//! view's `tauri::ipc::Channel` as raw bytes, the finish as one small JSON
//! message on the same Channel.
//!
//! A drain thread per attach reads the core's bounded receiver, joins the
//! output already waiting into one message of about 1 MiB at most and waits
//! while the webview holds two messages it has not acknowledged
//! (`terminal_ack`). A flood therefore back-pressures the PTY, not the
//! webview's main thread. The thread keeps the last 1 MiB it delivered: a
//! view shown again gets exactly that before the live output, so no byte
//! arrives twice or goes missing.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::ipc::{Channel, InvokeBody, InvokeResponseBody, Request};
use tauri::State;
use zerus_client_core::terminal::{
    self, status as texts, Attach, AttachEvent, Target, TerminalFacts,
};

/// Delivered output a view shown again redraws from (as the core's replay).
const REPLAY_BYTES: usize = 1 << 20;
/// Waiting output joins one message up to this size.
const MESSAGE_BYTES: usize = 1 << 20;
/// The most one core read holds (`AttachEvent::Output`).
const READ_BYTES: usize = 256 << 10;
/// Messages the webview may hold unacknowledged before delivery waits.
const IN_FLIGHT: usize = 2;
/// A webview that stops acknowledging (it reloads, say) holds delivery this long.
const ACK_WAIT: Duration = Duration::from_secs(1);

const DROP_CHANGED: &str =
    "Terminal changed during file transfer. Drop the files again in the intended session.";

/// `SessionsWindow::dropTerminalFiles` (2645-2659): a peer's guarded stage
/// identity, or this computer's paths when `computer` is empty.
#[derive(Clone, Debug, Deserialize)]
pub struct DropTarget {
    pub computer: String,
    pub name: String,
    pub machine: String,
    pub run_id: String,
    pub conversation_id: String,
    pub tracked: bool,
    pub created: i64,
}

/// Qt's `QDir::isAbsolutePath` check (SessionsWindow.cpp:1612). Peers return
/// POSIX paths; Windows clients additionally accept rooted drives and UNC.
fn transferred_absolute(path: &str, windows: bool) -> bool {
    if path.starts_with('/') {
        return true;
    }
    if !windows {
        return false;
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
    {
        return true;
    }
    path.strip_prefix("\\\\").is_some_and(|unc| {
        let mut parts = unc.split('\\');
        parts.next().is_some_and(|server| !server.is_empty())
            && parts.next().is_some_and(|share| !share.is_empty())
    })
}

/// The staged receipt's absolute, control-free paths, exactly `count`, as
/// Qt checks them before pasting (SessionsWindow.cpp:1609-1616).
pub fn staged_paths(receipt: &Value, count: usize) -> Result<Vec<String>, String> {
    let mut paths = Vec::new();
    for file in receipt["files"].as_array().into_iter().flatten() {
        let path = file["path"].as_str().unwrap_or("");
        if !transferred_absolute(path, cfg!(windows))
            || path.chars().any(|c| (c as u32) < 0x20 || c == '\u{7f}')
        {
            return Err("Invalid transferred file path".into());
        }
        paths.push(path.to_owned());
    }
    if paths.len() != count {
        return Err("Incomplete file transfer".into());
    }
    Ok(paths)
}

/// What a view attaches to: `computer` is "" for this computer, else the
/// alias; `machine` names the computer in the disconnected status.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalTarget {
    pub computer: String,
    pub name: String,
    pub run_id: String,
    pub machine: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Connected {
    pub generation: u64,
    pub status: String,
    /// The view's attach kept running; its delivered output came first.
    pub reused: bool,
}

/// Whether the selected session can connect, and the texts the view shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TerminalStatus {
    pub supported: bool,
    pub available: bool,
    pub reason: String,
    pub idle: String,
    pub connecting: String,
    pub disconnected: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct StatusInput {
    pub facts: TerminalFacts,
    /// The followed session's inspection (or phase 2's details), `{}` if none.
    #[serde(default)]
    pub details: Value,
    pub state: String,
    pub online: bool,
    #[serde(default)]
    pub restored_pending: bool,
    pub name: String,
    pub machine: String,
}

/// `terminal::available` and the status texts (`SessionsWindow.cpp:4021-4024`).
pub fn status(input: StatusInput) -> TerminalStatus {
    let available = terminal::available(
        &input.facts,
        &input.details,
        &input.state,
        input.online,
        input.restored_pending,
    );
    TerminalStatus {
        supported: terminal::supported(&input.facts, &input.details),
        available: available.is_ok(),
        reason: available.err().unwrap_or_default(),
        idle: texts::IDLE.into(),
        connecting: texts::connecting(&input.name),
        disconnected: texts::disconnected(&input.machine),
    }
}

/// Where a view's output goes: its Channel, or a test's collector.
pub trait Sink: Send + Sync {
    fn output(&self, bytes: Vec<u8>);
    fn message(&self, json: String);
}

struct ChannelSink(Channel);

impl Sink for ChannelSink {
    fn output(&self, bytes: Vec<u8>) {
        let _ = self.0.send(InvokeResponseBody::Raw(bytes));
    }
    fn message(&self, json: String) {
        let _ = self.0.send(InvokeResponseBody::Json(json));
    }
}

struct Delivery {
    sink: Arc<dyn Sink>,
    delivered: VecDeque<u8>,
    unacked: usize,
    /// The attach was stopped: nothing more is delivered, nothing is waited for.
    closed: bool,
}

struct Pipe {
    delivery: Mutex<Delivery>,
    acked: Condvar,
}

struct Open {
    generation: u64,
    target: TerminalTarget,
    attach: Attach,
    pipe: Arc<Pipe>,
    size: (u16, u16),
    resizes: usize,
    /// A hidden Terminal tab keeps its attach but cannot accept file drops.
    shown: bool,
}

struct Inner {
    hgs: PathBuf,
    allowed: bool,
    views: Mutex<HashMap<String, Open>>,
    /// Each view's last target, connected or not (`TerminalView`'s fields).
    targets: Mutex<HashMap<String, TerminalTarget>>,
    /// One connect at a time, so two never start an attach for one view.
    connecting: Mutex<()>,
    next: AtomicU64,
    /// Qt's one `m_terminalDropRequest` for the whole window.
    dropping: AtomicBool,
}

/// Release the transfer on every exit, including cancellation and unwinding.
struct Dropping<'a>(&'a AtomicBool);

impl Drop for Dropping<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The managed state of the terminal commands; clones share it.
#[derive(Clone)]
pub struct Terminals(Arc<Inner>);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Terminals {
    /// `allowed` is false in a replay without an explicit `ZERUS_CLIENT_HGS`.
    pub fn new(hgs: PathBuf, allowed: bool) -> Self {
        Self(Arc::new(Inner {
            hgs,
            allowed,
            views: Mutex::new(HashMap::new()),
            targets: Mutex::new(HashMap::new()),
            connecting: Mutex::new(()),
            next: AtomicU64::new(0),
            dropping: AtomicBool::new(false),
        }))
    }

    fn core(&self, target: &TerminalTarget) -> Target {
        Target {
            hgs: self.0.hgs.to_string_lossy().into_owned(),
            host: target.computer.clone(),
            name: target.name.clone(),
            run_id: target.run_id.clone(),
        }
    }

    /// The view's target after `next`: a kept run when the target did not change.
    fn remember(&self, view: &str, next: &TerminalTarget) -> (bool, TerminalTarget) {
        let mut targets = lock(&self.0.targets);
        let previous = targets.get(view).cloned();
        let changed = previous
            .as_ref()
            .is_none_or(|old| terminal::target_changed(&self.core(old), &self.core(next)));
        let mut kept = next.clone();
        if let Some(old) = previous.filter(|_| !changed) {
            kept.run_id = terminal::kept_run(&self.core(&old), &self.core(next));
        }
        targets.insert(view.to_owned(), kept.clone());
        (changed, kept)
    }

    /// Qt's `setSession`: true when the target changed, which stops the
    /// view's attach (the view clears its screen).
    pub fn retarget(&self, view: &str, target: &TerminalTarget) -> bool {
        // Not while a connect sets the view up.
        let _one = lock(&self.0.connecting);
        let (changed, kept) = self.remember(view, target);
        if changed {
            self.disconnect(view);
        } else if let Some(open) = lock(&self.0.views).get_mut(view) {
            open.target = kept;
        }
        changed
    }

    /// Connects the view, or rebinds its running attach to `sink` when the
    /// target did not change (a remounted or reloaded view).
    pub fn connect(
        &self,
        view: &str,
        target: TerminalTarget,
        cols: u16,
        rows: u16,
        sink: Arc<dyn Sink>,
    ) -> Result<Connected, String> {
        if !self.0.allowed {
            return Err(crate::actions::REPLAY_NEEDS_HGS.into());
        }
        let _one = lock(&self.0.connecting);
        let (changed, target) = self.remember(view, &target);
        if !changed {
            if let Some(connected) = self.rebind(view, &target, &sink, (cols, rows)) {
                return Ok(connected);
            }
        }
        // Stopping joins the attach's I/O thread: never under the views lock.
        let old = lock(&self.0.views).remove(view);
        if let Some(mut old) = old {
            old.close();
        }
        let (attach, events) = Attach::start(&self.core(&target), cols, rows)?;
        let generation = self.0.next.fetch_add(1, Ordering::Relaxed) + 1;
        let pipe = Arc::new(Pipe {
            delivery: Mutex::new(Delivery {
                sink,
                delivered: VecDeque::new(),
                unacked: 0,
                closed: false,
            }),
            acked: Condvar::new(),
        });
        self.drain(
            view.to_owned(),
            generation,
            events,
            Arc::clone(&pipe),
            target.machine.clone(),
        );
        let status = texts::connected(&target.name);
        lock(&self.0.views).insert(
            view.to_owned(),
            Open {
                generation,
                target,
                attach,
                pipe,
                size: (cols, rows),
                resizes: 0,
                shown: true,
            },
        );
        Ok(Connected {
            generation,
            status,
            reused: false,
        })
    }

    fn rebind(
        &self,
        view: &str,
        target: &TerminalTarget,
        sink: &Arc<dyn Sink>,
        size: (u16, u16),
    ) -> Option<Connected> {
        let mut views = lock(&self.0.views);
        let open = views.get_mut(view)?;
        open.target = target.clone();
        open.set_size(size.0, size.1);
        let mut delivery = lock(&open.pipe.delivery);
        let replay: Vec<u8> = delivery.delivered.iter().copied().collect();
        delivery.unacked = usize::from(!replay.is_empty());
        if !replay.is_empty() {
            // The view must finish parsing history before enabling PTY input.
            sink.message(json!({"kind": "replay"}).to_string());
            sink.output(replay);
        }
        delivery.sink = Arc::clone(sink);
        open.pipe.acked.notify_all();
        Some(Connected {
            generation: open.generation,
            status: texts::connected(&open.target.name),
            reused: true,
        })
    }

    fn drain(
        &self,
        view: String,
        generation: u64,
        events: Receiver<AttachEvent>,
        pipe: Arc<Pipe>,
        machine: String,
    ) {
        let inner = Arc::clone(&self.0);
        std::thread::Builder::new()
            .name("zerus-terminal".into())
            .spawn(move || {
                let finished = pump(&events, &pipe);
                // A stop reports nothing; only a finish does.
                let Some(code) = finished else { return };
                let message = json!({"kind": "finished", "code": code, "status": texts::finished(code, &machine)});
                {
                    let delivery = lock(&pipe.delivery);
                    if delivery.closed {
                        return;
                    }
                    delivery.sink.message(message.to_string());
                }
                let ended = {
                    let mut views = lock(&inner.views);
                    match views.get(&view) {
                        Some(open) if open.generation == generation => views.remove(&view),
                        _ => None,
                    }
                };
                drop(ended);
            })
            .expect("the terminal drain thread starts");
    }

    /// Queues input for the view's attach; a stale generation is ignored.
    pub fn write(&self, view: &str, generation: u64, bytes: &[u8]) -> Result<(), String> {
        match lock(&self.0.views).get(view) {
            Some(open) if open.generation == generation => open.attach.write(bytes),
            _ => Ok(()),
        }
    }

    pub fn generation(&self, view: &str) -> Option<u64> {
        lock(&self.0.views).get(view).map(|open| open.generation)
    }

    /// The tab's current visibility; an older attach's callback changes nothing.
    pub fn set_shown(&self, view: &str, generation: u64, shown: bool) {
        if let Some(open) = lock(&self.0.views)
            .get_mut(view)
            .filter(|open| open.generation == generation)
        {
            open.shown = shown;
        }
    }

    /// Dropped paths go only to the attach the drop named. Checking and
    /// queuing the paste share the views lock, so a reconnect cannot slip
    /// between them. No Enter is added (`pasteTerminalPaths`, 2625-2642).
    pub fn paste(&self, view: &str, generation: u64, text: &str) -> Result<(), String> {
        match lock(&self.0.views).get(view) {
            Some(open) if open.generation == generation && open.shown => {
                open.attach.write(text.as_bytes())
            }
            _ => Err(DROP_CHANGED.into()),
        }
    }

    /// `dropTerminalFiles` and its finish (SessionsWindow.cpp:2645-2659,
    /// 1602-1620), with a test dispatcher that never touches live sessions.
    async fn drop_files_with<F>(
        &self,
        view: &str,
        generation: u64,
        target: DropTarget,
        paths: Vec<String>,
        dispatch: impl FnOnce(zerus_client_core::Target, zerus_client_core::Action) -> F,
    ) -> Result<Option<String>, String>
    where
        F: std::future::Future<Output = Result<zerus_client_core::Outcome, String>>,
    {
        if self.generation(view) != Some(generation) {
            return Ok(None);
        }
        // Qt checks the transfer before even a local paste (2650-2651).
        if self.0.dropping.swap(true, Ordering::SeqCst) {
            return Ok(Some("Wait for the current file transfer to finish.".into()));
        }
        let _dropping = Dropping(&self.0.dropping);
        if target.computer.is_empty() {
            self.paste(
                view,
                generation,
                &terminal::paste_paths(&paths, cfg!(windows))?,
            )?;
            return Ok(None);
        }
        let untracked =
            target.run_id.is_empty() && target.conversation_id.is_empty() && !target.tracked;
        if target.run_id.is_empty() && !untracked {
            return Err("Wait for the session identity to load, then drop the files again.".into());
        }
        let staged = dispatch(
            zerus_client_core::Target {
                computer: target.computer,
                name: target.name,
                run_id: target.run_id,
                conversation_id: target.conversation_id,
            },
            zerus_client_core::Action::StageFiles {
                paths: paths.iter().map(PathBuf::from).collect(),
                expected_created: untracked.then_some(target.created),
            },
        )
        .await;
        let receipt = match staged {
            Ok(zerus_client_core::Outcome::Done { receipt, .. }) => receipt,
            Err(detail) if detail == crate::actions::BUSY => {
                return Ok(Some("Wait for the session update to finish.".into()));
            }
            Ok(
                zerus_client_core::Outcome::Failed { detail }
                | zerus_client_core::Outcome::Uncertain { detail },
            )
            | Err(detail) => {
                if untracked && detail.contains("no saved binding") {
                    return Err(format!(
                        "Update hgs on {} to drop files into an untracked session.",
                        target.machine
                    ));
                }
                return Err(detail);
            }
        };
        if self.generation(view) != Some(generation) {
            return Err(DROP_CHANGED.into());
        }
        let staged = staged_paths(&receipt, paths.len())?;
        // Every peer uses POSIX quoting, even from a Windows client.
        self.paste(view, generation, &terminal::paste_paths(&staged, false)?)?;
        Ok(None)
    }

    /// Applies a new size; an unchanged size never reaches the PTY.
    pub fn resize(&self, view: &str, generation: u64, cols: u16, rows: u16) {
        let mut views = lock(&self.0.views);
        if let Some(open) = views
            .get_mut(view)
            .filter(|open| open.generation == generation)
        {
            open.set_size(cols, rows);
        }
    }

    /// Resizes applied to the view's attach so far.
    #[cfg(all(test, unix))]
    fn resizes(&self, view: &str) -> usize {
        lock(&self.0.views).get(view).map_or(0, |open| open.resizes)
    }

    /// The webview wrote one output message to its screen.
    pub fn ack(&self, view: &str, generation: u64) {
        let views = lock(&self.0.views);
        if let Some(open) = views.get(view).filter(|open| open.generation == generation) {
            let mut delivery = lock(&open.pipe.delivery);
            delivery.unacked = delivery.unacked.saturating_sub(1);
            open.pipe.acked.notify_all();
        }
    }

    /// Hangs up the view's attach; the session keeps running. The status
    /// text, or None when nothing was connected.
    pub fn disconnect(&self, view: &str) -> Option<String> {
        let old = lock(&self.0.views).remove(view);
        old.map(|mut open| {
            open.close();
            texts::disconnected(&open.target.machine)
        })
    }

    /// The window closed: every attach hangs up.
    pub fn stop_all(&self) {
        let all: Vec<Open> = lock(&self.0.views).drain().map(|(_, open)| open).collect();
        for mut open in all {
            open.close();
        }
    }
}

impl Open {
    /// Applies a new size; an unchanged size never reaches the PTY.
    fn set_size(&mut self, cols: u16, rows: u16) {
        if self.size != (cols, rows) {
            self.size = (cols, rows);
            self.resizes += 1;
            self.attach.resize(cols, rows);
        }
    }

    /// Hangs up: delivery ends first, so buffered output never reaches the
    /// view's sink and nothing waits for its acknowledgements.
    fn close(&mut self) {
        lock(&self.pipe.delivery).closed = true;
        self.pipe.acked.notify_all();
        self.attach.stop();
    }
}

/// Joins the core's events into messages of at most `MESSAGE_BYTES` and
/// delivers them. Returns the exit code of a finish, None after a stop.
fn pump(events: &Receiver<AttachEvent>, pipe: &Pipe) -> Option<i32> {
    let mut finished = None;
    while finished.is_none() {
        let Ok(first) = events.recv() else { break };
        let mut chunk = Vec::new();
        let mut next = Some(first);
        while let Some(event) = next.take() {
            match event {
                AttachEvent::Output { data } => chunk.extend_from_slice(&data),
                AttachEvent::Finished { code } => finished = Some(code),
            }
            if finished.is_none() && chunk.len() + READ_BYTES <= MESSAGE_BYTES {
                next = events.try_recv().ok();
            }
        }
        if !chunk.is_empty() && !deliver(pipe, chunk) {
            return None;
        }
    }
    finished
}

/// False when the attach was stopped: the chunk is dropped.
fn deliver(pipe: &Pipe, chunk: Vec<u8>) -> bool {
    let deadline = Instant::now() + ACK_WAIT;
    let mut delivery = lock(&pipe.delivery);
    while delivery.unacked >= IN_FLIGHT && !delivery.closed {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        delivery = pipe
            .acked
            .wait_timeout(delivery, left)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
    if delivery.closed {
        return false;
    }
    delivery.delivered.extend(chunk.iter().copied());
    let excess = delivery.delivered.len().saturating_sub(REPLAY_BYTES);
    delivery.delivered.drain(..excess);
    delivery.unacked += 1;
    delivery.sink.output(chunk);
    true
}

fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> impl std::future::Future<Output = Result<T, String>> {
    let task = tauri::async_runtime::spawn_blocking(work);
    async move { task.await.map_err(|error| error.to_string()) }
}

/// `Ok(Some(..))` is an info notice, `Err(..)` a sticky one. The frontend
/// alone shows the transfer-in-progress notice. A stage is never replayed.
#[tauri::command(rename_all = "snake_case")]
#[allow(clippy::too_many_arguments)] // Managed state and the drop's IPC arguments.
pub async fn terminal_drop(
    webview: tauri::Webview,
    terminals: State<'_, Terminals>,
    actions: State<'_, crate::actions::Actions>,
    view: String,
    generation: u64,
    target: DropTarget,
    paths: Vec<String>,
) -> Result<Option<String>, String> {
    drop_origin(webview.label(), &view)?;
    let (terminals, actions) = (terminals.inner().clone(), actions.inner().clone());
    blocking(move || {
        tauri::async_runtime::block_on(terminals.drop_files_with(
            &view,
            generation,
            target,
            paths,
            |target, action| crate::actions::dispatch(&actions, target, action),
        ))
    })
    .await?
}

fn drop_origin(origin: &str, view: &str) -> Result<(), String> {
    if origin != crate::MAIN_WINDOW || view != crate::MAIN_WINDOW {
        return Err("Terminal drops are available only in the main view.".into());
    }
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn terminal_shown(
    webview: tauri::Webview,
    state: State<'_, Terminals>,
    view: String,
    generation: u64,
    shown: bool,
) -> Result<(), String> {
    drop_origin(webview.label(), &view)?;
    state.set_shown(&view, generation, shown);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn terminal_connect(
    state: State<'_, Terminals>,
    view: String,
    target: TerminalTarget,
    cols: u16,
    rows: u16,
    on_event: Channel,
) -> Result<Connected, String> {
    let terminals = state.inner().clone();
    blocking(move || terminals.connect(&view, target, cols, rows, Arc::new(ChannelSink(on_event))))
        .await?
}

/// Input as a raw body: `x-zerus-view` and `x-zerus-generation` name the attach.
#[tauri::command]
pub async fn terminal_write(
    state: State<'_, Terminals>,
    request: Request<'_>,
) -> Result<(), String> {
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
    };
    let view = header("x-zerus-view").ok_or("missing x-zerus-view")?;
    let generation = header("x-zerus-generation")
        .and_then(|value| value.parse().ok())
        .ok_or("missing x-zerus-generation")?;
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err("terminal input must be a raw body".into());
    };
    state.write(view, generation, bytes)
}

#[tauri::command(rename_all = "snake_case")]
pub fn terminal_resize(
    state: State<'_, Terminals>,
    view: String,
    generation: u64,
    cols: u16,
    rows: u16,
) {
    state.resize(&view, generation, cols, rows);
}

#[tauri::command(rename_all = "snake_case")]
pub fn terminal_ack(state: State<'_, Terminals>, view: String, generation: u64) {
    state.ack(&view, generation);
}

#[tauri::command]
pub async fn terminal_disconnect(
    state: State<'_, Terminals>,
    view: String,
) -> Result<Option<String>, String> {
    let terminals = state.inner().clone();
    blocking(move || terminals.disconnect(&view)).await
}

#[tauri::command]
pub async fn terminal_retarget(
    state: State<'_, Terminals>,
    view: String,
    target: TerminalTarget,
) -> Result<bool, String> {
    let terminals = state.inner().clone();
    blocking(move || terminals.retarget(&view, &target)).await
}

#[tauri::command]
pub fn terminal_status(input: StatusInput) -> TerminalStatus {
    status(input)
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn staged_receipts_are_checked_as_qt_does() {
        let receipt = json!({"files": [{"path": "/home/example/.hgs/drop/a.txt"}, {"path": "/home/example/.hgs/drop/b c.txt"}]});
        assert_eq!(
            staged_paths(&receipt, 2).unwrap(),
            [
                "/home/example/.hgs/drop/a.txt",
                "/home/example/.hgs/drop/b c.txt"
            ]
        );
        for malformed in [json!({}), json!({"files": {}}), json!({"files": []})] {
            assert_eq!(
                staged_paths(&malformed, 1),
                Err("Incomplete file transfer".into())
            );
        }
        assert_eq!(
            staged_paths(&receipt, 3),
            Err("Incomplete file transfer".into())
        );
        assert_eq!(
            staged_paths(&receipt, 1),
            Err("Incomplete file transfer".into())
        );
        for path in [
            "relative.txt",
            "C:relative.txt",
            "/home/example/a\nb",
            "/home/example/a\u{1f}b",
            "/home/example/a\u{7f}b",
            "/home/example/a\0b",
        ] {
            assert_eq!(
                staged_paths(&json!({"files": [{"path": path}]}), 1),
                Err("Invalid transferred file path".into()),
                "{path:?}"
            );
        }
        assert_eq!(
            staged_paths(&json!({"files": [{}]}), 1),
            Err("Invalid transferred file path".into())
        );
        assert_eq!(
            staged_paths(&json!({"files": [{"path": 42}]}), 1),
            Err("Invalid transferred file path".into())
        );
    }

    #[test]
    fn transferred_paths_follow_the_clients_platform() {
        for windows in [false, true] {
            assert!(transferred_absolute("/home/example/staged.txt", windows));
            for path in [
                "relative.txt",
                "C:relative.txt",
                "\\root-relative.txt",
                "1:\\file",
                "C:",
                "C|/file",
                r"\\server",
                r"\\\share",
                r"\\server\",
            ] {
                assert!(
                    !transferred_absolute(path, windows),
                    "{path:?} windows={windows}"
                );
            }
            for path in [
                "C:\\Users\\example\\staged.txt",
                "Z:/drop/staged.txt",
                "\\\\server\\share\\staged.txt",
            ] {
                assert_eq!(transferred_absolute(path, windows), windows, "{path:?}");
            }
        }
    }

    #[test]
    fn only_the_main_webview_can_drop_into_the_main_terminal() {
        assert_eq!(drop_origin("main", "main"), Ok(()));
        for (origin, view) in [
            ("native-ui", "main"),
            ("main", "native-ui"),
            ("native-ui", "native-ui"),
        ] {
            assert_eq!(
                drop_origin(origin, view),
                Err("Terminal drops are available only in the main view.".into())
            );
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::Path;

    /// What a view received; `ack` mirrors the webview acknowledging each output.
    #[derive(Default)]
    struct Collect {
        seen: Mutex<(Vec<Vec<u8>>, Vec<Value>)>,
        changed: Condvar,
    }

    impl Sink for Collect {
        fn output(&self, bytes: Vec<u8>) {
            lock(&self.seen).0.push(bytes);
            self.changed.notify_all();
        }
        fn message(&self, json: String) {
            lock(&self.seen)
                .1
                .push(serde_json::from_str(&json).unwrap());
            self.changed.notify_all();
        }
    }

    impl Collect {
        fn text(&self) -> String {
            String::from_utf8_lossy(&lock(&self.seen).0.concat()).into_owned()
        }
        fn outputs(&self) -> usize {
            lock(&self.seen).0.len()
        }
        fn messages(&self) -> Vec<Value> {
            lock(&self.seen).1.clone()
        }
        fn wait_after(&self, observed: usize, timeout: Duration) {
            let seen = lock(&self.seen);
            if seen.0.len() == observed {
                // Check under the same lock as output publication, so an
                // arriving frame cannot lose its wake before this wait.
                drop(
                    self.changed
                        .wait_timeout(seen, timeout)
                        .unwrap_or_else(PoisonError::into_inner),
                );
            }
        }
    }

    fn terminals() -> Terminals {
        Terminals::new(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests/support/fake_attach.py"),
            true,
        )
    }

    fn target(run: &str) -> TerminalTarget {
        TerminalTarget {
            computer: "mac".into(),
            name: "claude/sample-project/review".into(),
            run_id: run.into(),
            machine: "studio".into(),
        }
    }

    /// Waits up to ten seconds for `done`, acknowledging every output as it arrives.
    fn until(t: &Terminals, generation: u64, sink: &Collect, done: impl Fn(&Collect) -> bool) {
        until_with_timeout(t, generation, sink, Duration::from_secs(10), done);
    }

    fn until_with_timeout(
        t: &Terminals,
        generation: u64,
        sink: &Collect,
        timeout: Duration,
        done: impl Fn(&Collect) -> bool,
    ) {
        let deadline = Instant::now() + timeout;
        let mut acked = 0;
        while !done(sink) {
            assert!(Instant::now() < deadline, "timed out: {:?}", sink.text());
            // Acknowledgements can deliver more output immediately. Advance only
            // through this snapshot; later frames still need their own ACK.
            let observed = sink.outputs();
            for _ in acked..observed {
                t.ack("main", generation);
            }
            acked = observed;
            // ACK promptly when another frame arrives. A fixed sleep here
            // rate-limits small PTY reads before the next ACK can be delivered.
            sink.wait_after(observed, Duration::from_millis(10));
        }
    }

    /// Waits for the fake's hello line: input written before it is flushed
    /// when the fake switches the terminal to raw mode.
    fn hello(t: &Terminals, generation: u64, sink: &Collect) {
        until(t, generation, sink, |s| s.text().contains("\"COLORTERM\""));
        until(t, generation, sink, |s| s.text().ends_with("\r\n"));
    }

    #[test]
    fn a_paste_goes_to_the_current_attach() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let c = t
            .connect("main", target("run-1"), 80, 24, sink.clone())
            .unwrap();
        hello(&t, c.generation, &sink);
        assert_eq!(t.generation("main"), Some(c.generation));
        t.write("main", c.generation, b"echo ").unwrap();
        let text = terminal::paste_paths(&["/home/example/it's.txt".to_owned()], false).unwrap();
        t.paste("main", c.generation, &text).unwrap();
        t.write("main", c.generation, b"\r").unwrap();
        until(&t, c.generation, &sink, |s| {
            s.text().contains("echo '/home/example/it'\\''s.txt'")
        });
        t.disconnect("main");
        assert_eq!(t.generation("main"), None);
        assert_eq!(t.paste("main", c.generation, "x"), Err("Terminal changed during file transfer. Drop the files again in the intended session.".into()));
    }

    fn drop_target() -> DropTarget {
        DropTarget {
            computer: "studio".into(),
            name: "claude/sample-project/review".into(),
            machine: "studio".into(),
            run_id: "run-1".into(),
            conversation_id: "conversation-1".into(),
            tracked: true,
            created: 1234,
        }
    }

    fn dropped_paths() -> Vec<String> {
        vec!["/home/example/source's.txt".into()]
    }

    fn stage_done() -> zerus_client_core::Outcome {
        zerus_client_core::Outcome::Done {
            receipt: json!({"files": [{"path": "/home/example/staged's.txt"}]}),
            cleanup: None,
        }
    }

    fn drop_with(
        t: &Terminals,
        generation: u64,
        target: DropTarget,
        dispatch: impl FnOnce(
            zerus_client_core::Target,
            zerus_client_core::Action,
        ) -> Result<zerus_client_core::Outcome, String>,
    ) -> Result<Option<String>, String> {
        let mut future = Box::pin(t.drop_files_with(
            "main",
            generation,
            target,
            dropped_paths(),
            |target, action| std::future::ready(dispatch(target, action)),
        ));
        match std::future::Future::poll(
            future.as_mut(),
            &mut std::task::Context::from_waker(std::task::Waker::noop()),
        ) {
            std::task::Poll::Ready(result) => result,
            std::task::Poll::Pending => {
                panic!("the synchronous test dispatcher must finish in one poll")
            }
        }
    }

    #[test]
    fn a_stale_drop_stages_and_pastes_nothing() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let c = t
            .connect("main", target("run-1"), 80, 24, sink.clone())
            .unwrap();
        hello(&t, c.generation, &sink);
        assert_eq!(
            drop_with(&t, c.generation + 1, drop_target(), |_, _| panic!(
                "must not stage"
            )),
            Ok(None)
        );
        t.write("main", c.generation, b"echo probe\r").unwrap();
        until(&t, c.generation, &sink, |s| s.text().contains("echo probe"));
        assert!(!sink.text().contains("staged"));
        t.disconnect("main");
    }

    #[test]
    fn local_drops_paste_quoted_paths_without_staging_or_enter() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let c = t
            .connect("main", target("run-1"), 80, 24, sink.clone())
            .unwrap();
        hello(&t, c.generation, &sink);
        let mut local = drop_target();
        local.computer.clear();
        t.write("main", c.generation, b"echo ").unwrap();
        assert_eq!(
            drop_with(&t, c.generation, local, |_, _| panic!(
                "local files never stage"
            )),
            Ok(None)
        );
        t.write("main", c.generation, b"end\r").unwrap();
        until(&t, c.generation, &sink, |s| s.text().contains("end"));
        assert!(sink
            .text()
            .contains("echo '/home/example/source'\\''s.txt' end"));
        t.disconnect("main");
    }

    #[test]
    fn a_peer_drop_stages_once_with_exact_identity_then_pastes_posix_paths() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let c = t
            .connect("main", target("run-1"), 80, 24, sink.clone())
            .unwrap();
        hello(&t, c.generation, &sink);
        t.write("main", c.generation, b"echo ").unwrap();
        let mut calls = 0;
        assert_eq!(
            drop_with(&t, c.generation, drop_target(), |target, action| {
                calls += 1;
                assert_eq!(
                    target,
                    zerus_client_core::Target {
                        computer: "studio".into(),
                        name: "claude/sample-project/review".into(),
                        run_id: "run-1".into(),
                        conversation_id: "conversation-1".into()
                    }
                );
                assert!(
                    matches!(action, zerus_client_core::Action::StageFiles { paths, expected_created: None } if paths == dropped_paths().iter().map(PathBuf::from).collect::<Vec<_>>())
                );
                Ok(stage_done())
            }),
            Ok(None)
        );
        assert_eq!(calls, 1);
        t.write("main", c.generation, b"end\r").unwrap();
        until(&t, c.generation, &sink, |s| s.text().contains("end"));
        assert!(sink
            .text()
            .contains("echo '/home/example/staged'\\''s.txt' end"));
        assert!(!sink.text().contains("source"));
        t.disconnect("main");
    }

    #[test]
    fn only_an_untracked_drop_uses_creation_identity() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let c = t.connect("main", target("run-1"), 80, 24, sink).unwrap();
        let mut target = drop_target();
        target.run_id.clear();
        for tracked in [true, false] {
            target.tracked = tracked;
            assert_eq!(
                drop_with(&t, c.generation, target.clone(), |_, _| panic!(
                    "incomplete identity never stages"
                )),
                Err("Wait for the session identity to load, then drop the files again.".into())
            );
        }
        target.conversation_id.clear();
        target.tracked = true;
        assert_eq!(
            drop_with(&t, c.generation, target.clone(), |_, _| panic!(
                "tracked identity never stages"
            )),
            Err("Wait for the session identity to load, then drop the files again.".into())
        );
        target.tracked = false;
        assert_eq!(
            drop_with(&t, c.generation, target.clone(), |target, action| {
                assert!(target.run_id.is_empty() && target.conversation_id.is_empty());
                assert!(matches!(
                    action,
                    zerus_client_core::Action::StageFiles {
                        expected_created: Some(1234),
                        ..
                    }
                ));
                Err("no saved binding".into())
            }),
            Err("Update hgs on studio to drop files into an untracked session.".into())
        );
        assert!(!t.0.dropping.load(Ordering::SeqCst));
        t.disconnect("main");
    }

    #[test]
    fn failed_and_uncertain_stages_never_paste_or_replay() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let c = t
            .connect("main", target("run-1"), 80, 24, sink.clone())
            .unwrap();
        hello(&t, c.generation, &sink);
        let mut calls = 0;
        for result in [
            Ok(zerus_client_core::Outcome::Failed {
                detail: "failed".into(),
            }),
            Ok(zerus_client_core::Outcome::Uncertain {
                detail: "uncertain".into(),
            }),
            Err("dispatch error".into()),
        ] {
            let expected = match &result {
                Ok(
                    zerus_client_core::Outcome::Failed { detail }
                    | zerus_client_core::Outcome::Uncertain { detail },
                )
                | Err(detail) => detail.clone(),
                _ => unreachable!(),
            };
            assert_eq!(
                drop_with(&t, c.generation, drop_target(), |_, _| {
                    calls += 1;
                    result
                }),
                Err(expected)
            );
            assert!(!t.0.dropping.load(Ordering::SeqCst));
        }
        assert_eq!(calls, 3);
        t.write("main", c.generation, b"echo probe\r").unwrap();
        until(&t, c.generation, &sink, |s| s.text().contains("echo probe"));
        assert!(!sink.text().contains("source") && !sink.text().contains("staged"));
        t.disconnect("main");
    }

    #[test]
    fn a_reconnect_during_staging_never_pastes_into_the_new_attach() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let first = Arc::new(Collect::default());
        let c = t.connect("main", target("run-1"), 80, 24, first).unwrap();
        let second = Arc::new(Collect::default());
        let mut next = None;
        assert_eq!(drop_with(&t, c.generation, drop_target(), |_, _| {
            next = Some(t.connect("main", target("run-2"), 80, 24, second.clone()).unwrap());
            Ok(stage_done())
        }), Err("Terminal changed during file transfer. Drop the files again in the intended session.".into()));
        let next = next.unwrap();
        hello(&t, next.generation, &second);
        assert_eq!(
            t.paste("main", c.generation, "stale"),
            Err(DROP_CHANGED.into())
        );
        t.write("main", next.generation, b"echo probe\r").unwrap();
        until(&t, next.generation, &second, |s| {
            s.text().contains("echo probe")
        });
        assert!(!second.text().contains("staged"));
        assert!(!t.0.dropping.load(Ordering::SeqCst));
        t.disconnect("main");
    }

    #[test]
    fn a_transfer_blocks_other_peer_and_local_drops() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let c = t
            .connect("main", target("run-1"), 80, 24, sink.clone())
            .unwrap();
        hello(&t, c.generation, &sink);
        assert_eq!(
            drop_with(&t, c.generation, drop_target(), |_, _| {
                let mut local = drop_target();
                local.computer.clear();
                for other in [drop_target(), local] {
                    assert_eq!(
                        drop_with(&t, c.generation, other, |_, _| panic!(
                            "a second transfer never starts"
                        )),
                        Ok(Some("Wait for the current file transfer to finish.".into()))
                    );
                    assert!(
                        t.0.dropping.load(Ordering::SeqCst),
                        "refused drops cannot release the first claim"
                    );
                }
                Ok(stage_done())
            }),
            Ok(None)
        );
        t.write("main", c.generation, b"\recho probe\r").unwrap();
        until(&t, c.generation, &sink, |s| s.text().contains("echo probe"));
        assert!(!sink.text().contains("source"));
        assert!(!t.0.dropping.load(Ordering::SeqCst));
        t.disconnect("main");
    }

    #[test]
    fn a_dispatch_panic_releases_the_transfer_claim() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let c = t.connect("main", target("run-1"), 80, 24, sink).unwrap();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop_with(&t, c.generation, drop_target(), |_, _| {
                panic!("synthetic dispatch panic")
            })
        }));
        assert!(panic.is_err());
        assert!(!t.0.dropping.load(Ordering::SeqCst));
        assert_eq!(
            drop_with(&t, c.generation, drop_target(), |_, _| Err(
                "next transfer reached dispatch".into()
            )),
            Err("next transfer reached dispatch".into())
        );
        t.disconnect("main");
    }

    #[test]
    fn cancelling_a_drop_releases_the_transfer_claim() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let c = t.connect("main", target("run-1"), 80, 24, sink).unwrap();
        let mut pending = Box::pin(t.drop_files_with(
            "main",
            c.generation,
            drop_target(),
            dropped_paths(),
            |_, _| std::future::pending(),
        ));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(std::future::Future::poll(pending.as_mut(), &mut context).is_pending());
        assert!(t.0.dropping.load(Ordering::SeqCst));
        drop(pending);
        assert!(!t.0.dropping.load(Ordering::SeqCst));
        assert_eq!(
            drop_with(&t, c.generation, drop_target(), |_, _| Err(
                "next transfer reached dispatch".into()
            )),
            Err("next transfer reached dispatch".into())
        );
        t.disconnect("main");
    }

    #[test]
    fn an_invalid_receipt_pastes_nothing_and_releases_the_claim() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let c = t
            .connect("main", target("run-1"), 80, 24, sink.clone())
            .unwrap();
        hello(&t, c.generation, &sink);
        for receipt in [
            json!({"files": []}),
            json!({"files": [{"path": "relative.txt"}]}),
        ] {
            assert!(drop_with(&t, c.generation, drop_target(), |_, _| Ok(
                zerus_client_core::Outcome::Done {
                    receipt,
                    cleanup: None
                }
            ))
            .is_err());
            assert!(!t.0.dropping.load(Ordering::SeqCst));
        }
        t.write("main", c.generation, b"echo probe\r").unwrap();
        until(&t, c.generation, &sink, |s| s.text().contains("echo probe"));
        t.disconnect("main");
    }

    #[test]
    fn a_tracked_drop_keeps_a_no_saved_binding_failure_unchanged() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let c = t
            .connect(
                "main",
                target("run-1"),
                80,
                24,
                Arc::new(Collect::default()),
            )
            .unwrap();
        assert_eq!(
            drop_with(&t, c.generation, drop_target(), |_, _| Ok(
                zerus_client_core::Outcome::Failed {
                    detail: "no saved binding".into()
                }
            )),
            Err("no saved binding".into())
        );
        t.disconnect("main");
    }

    #[test]
    fn a_reconnect_is_reported_before_an_invalid_receipt() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let c = t
            .connect(
                "main",
                target("run-1"),
                80,
                24,
                Arc::new(Collect::default()),
            )
            .unwrap();
        assert_eq!(
            drop_with(&t, c.generation, drop_target(), |_, _| {
                t.connect(
                    "main",
                    target("run-2"),
                    80,
                    24,
                    Arc::new(Collect::default()),
                )
                .unwrap();
                Ok(zerus_client_core::Outcome::Done {
                    receipt: json!({"files": [{"path": "relative.txt"}]}),
                    cleanup: None,
                })
            }),
            Err(DROP_CHANGED.into())
        );
        t.disconnect("main");
    }

    #[test]
    fn a_taken_write_lane_is_an_info_notice_and_is_never_replayed() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let c = t
            .connect(
                "main",
                target("run-1"),
                80,
                24,
                Arc::new(Collect::default()),
            )
            .unwrap();
        let mut calls = 0;
        assert_eq!(
            drop_with(&t, c.generation, drop_target(), |_, _| {
                calls += 1;
                Err(crate::actions::BUSY.into())
            }),
            Ok(Some("Wait for the session update to finish.".into()))
        );
        assert_eq!(calls, 1);
        assert!(!t.0.dropping.load(Ordering::SeqCst));
        t.disconnect("main");
    }

    #[test]
    fn a_stage_finishing_while_the_terminal_tab_is_hidden_pastes_nothing() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let c = t
            .connect("main", target("run-1"), 80, 24, sink.clone())
            .unwrap();
        hello(&t, c.generation, &sink);
        assert_eq!(
            drop_with(&t, c.generation, drop_target(), |_, _| {
                t.set_shown("main", c.generation, false);
                Ok(stage_done())
            }),
            Err(DROP_CHANGED.into())
        );
        assert_eq!(
            t.paste("main", c.generation, "hidden"),
            Err(DROP_CHANGED.into())
        );
        t.set_shown("main", c.generation, true);
        t.write("main", c.generation, b"echo probe\r").unwrap();
        until(&t, c.generation, &sink, |s| s.text().contains("echo probe"));
        assert!(!sink.text().contains("staged") && !sink.text().contains("hidden"));
        t.disconnect("main");
    }

    #[test]
    fn a_stale_tab_visibility_update_never_changes_the_current_attach() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let first = t
            .connect(
                "main",
                target("run-1"),
                80,
                24,
                Arc::new(Collect::default()),
            )
            .unwrap();
        let current = t
            .connect("main", target("run-2"), 80, 24, sink.clone())
            .unwrap();
        hello(&t, current.generation, &sink);
        t.set_shown("main", first.generation, false);
        t.write("main", current.generation, b"echo ").unwrap();
        t.paste("main", current.generation, "shown").unwrap();
        t.write("main", current.generation, b"\r").unwrap();
        until(&t, current.generation, &sink, |s| {
            s.text().contains("echo shown")
        });
        t.disconnect("main");
    }

    #[test]
    fn connect_runs_qts_attach_call() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let c = t
            .connect("main", target("run-1"), 90, 30, sink.clone())
            .unwrap();
        assert_eq!(c.status, "Connected: claude/sample-project/review");
        assert!(!c.reused);
        until(&t, c.generation, &sink, |s| s.text().contains("\r\n"));
        let hello: Value = serde_json::from_str(sink.text().split("\r\n").next().unwrap()).unwrap();
        assert_eq!(
            hello["argv"],
            json!([
                "@mac",
                "a",
                "claude/sample-project/review",
                "--existing",
                "--run-id",
                "run-1"
            ])
        );
        assert_eq!(
            (hello["cols"].as_u64(), hello["rows"].as_u64()),
            (Some(90), Some(30))
        );
        assert_eq!(hello["env"]["HGS_TAB"], "0");
        assert_eq!(
            t.disconnect("main").as_deref(),
            Some("Disconnected — the session continues on studio")
        );
    }

    #[test]
    fn a_view_shown_again_redraws_without_doubling() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let first = Arc::new(Collect::default());
        let c = t
            .connect("main", target("run-1"), 80, 24, first.clone())
            .unwrap();
        hello(&t, c.generation, &first);
        t.write("main", c.generation, b"echo one\r").unwrap();
        until(&t, c.generation, &first, |s| s.text().contains("echo one"));
        let second = Arc::new(Collect::default());
        let again = t
            .connect("main", target("run-1"), 80, 24, second.clone())
            .unwrap();
        assert!(again.reused);
        assert_eq!(second.messages(), [json!({"kind": "replay"})]);
        assert_eq!(again.generation, c.generation);
        assert_eq!(
            second.text(),
            first.text(),
            "the redraw is exactly what was delivered"
        );
        let before = first.outputs();
        t.write("main", c.generation, b"echo two\r").unwrap();
        until(&t, c.generation, &second, |s| s.text().contains("echo two"));
        assert_eq!(second.text().matches("echo two").count(), 1);
        assert_eq!(first.outputs(), before, "the hidden view gets nothing more");
        t.disconnect("main");
    }

    #[test]
    fn a_changed_target_disconnects_and_a_rename_does_not() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        assert!(
            t.retarget("main", &target("run-1")),
            "a first target is new"
        );
        let c = t
            .connect("main", target("run-1"), 80, 24, sink.clone())
            .unwrap();
        until(&t, c.generation, &sink, |s| s.text().contains("\r\n"));
        let mut renamed = target("run-1");
        renamed.name = "claude/sample-project/review-2".into();
        assert!(
            !t.retarget("main", &renamed),
            "a rename of the same run keeps the attach"
        );
        let mut omitted = renamed.clone();
        omitted.run_id.clear();
        assert!(
            !t.retarget("main", &omitted),
            "an inspection without the run keeps it"
        );
        assert!(t.retarget("main", &target("run-2")));
        assert_eq!(
            t.disconnect("main"),
            None,
            "the changed target already stopped it"
        );
    }

    #[test]
    fn identical_sizes_never_reach_the_pty() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let c = t
            .connect("main", target("run-1"), 80, 24, sink.clone())
            .unwrap();
        for _ in 0..50 {
            t.resize("main", c.generation, 80, 24);
        }
        assert_eq!(t.resizes("main"), 0);
        t.resize("main", c.generation, 100, 30);
        t.resize("main", c.generation, 100, 30);
        assert_eq!(t.resizes("main"), 1);
        t.resize("main", c.generation + 1, 120, 40);
        assert_eq!(t.resizes("main"), 1, "a stale generation changes nothing");
        hello(&t, c.generation, &sink);
        t.write("main", c.generation, b"size\r").unwrap();
        until(&t, c.generation, &sink, |s| {
            s.text().contains("size 100 30")
        });
        t.disconnect("main");
    }

    #[test]
    fn a_paste_over_the_queue_bound_queues_nothing() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let c = t
            .connect("main", target("run-1"), 80, 24, sink.clone())
            .unwrap();
        assert_eq!(
            t.write("main", c.generation, &vec![b'x'; 5 << 20]),
            Err("Terminal input queue is full; wait before pasting more text.".into())
        );
        hello(&t, c.generation, &sink);
        t.write("main", c.generation, b"echo ok\r").unwrap();
        until(&t, c.generation, &sink, |s| s.text().contains("echo ok"));
        t.disconnect("main");
    }

    #[test]
    fn a_flood_waits_for_acknowledgements_and_loses_nothing() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let c = t
            .connect("main", target("run-1"), 80, 24, sink.clone())
            .unwrap();
        hello(&t, c.generation, &sink);
        std::thread::sleep(Duration::from_millis(50));
        ack_all(&t, c.generation, &sink);
        let start = sink.outputs();
        t.write("main", c.generation, b"flood 3000000\r").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            sink.outputs() - start,
            2,
            "exactly two unacknowledged messages, then the drain waits"
        );
        until(&t, c.generation, &sink, |s| s.text().contains("done"));
        let text = sink.text();
        let flood = text.split_once("}}\r\n").unwrap().1;
        assert_eq!(
            flood.matches('x').count(),
            3_000_000,
            "the hello line holds two x of its own"
        );
        t.disconnect("main");
    }

    #[test]
    fn the_finish_reports_qts_status_and_forgets_the_view() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let c = t
            .connect("main", target("run-1"), 80, 24, sink.clone())
            .unwrap();
        hello(&t, c.generation, &sink);
        t.write("main", c.generation, b"exit 2\r").unwrap();
        until(&t, c.generation, &sink, |s| !s.messages().is_empty());
        assert_eq!(
            sink.messages()[0],
            json!({"kind": "finished", "code": 2,
                   "status": "Attach client exited (2) — session output is preserved above"})
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while lock(&t.0.views).contains_key("main") {
            assert!(
                Instant::now() < deadline,
                "the finished attach is forgotten"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(t.disconnect("main"), None);
    }

    fn ack_all(t: &Terminals, generation: u64, sink: &Collect) {
        for _ in 0..sink.outputs() {
            t.ack("main", generation);
        }
    }

    /// Connects, acknowledges the hello and starts a 3 MB flood, holding every
    /// acknowledgement once two messages arrived.
    fn held_flood(t: &Terminals, sink: &Arc<Collect>) -> (u64, usize) {
        let c = t
            .connect("main", target("run-1"), 80, 24, sink.clone())
            .unwrap();
        hello(t, c.generation, sink);
        std::thread::sleep(Duration::from_millis(50));
        ack_all(t, c.generation, sink);
        let start = sink.outputs();
        t.write("main", c.generation, b"flood 3000000\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while sink.outputs() < start + 2 {
            assert!(Instant::now() < deadline, "the flood starts");
            std::thread::sleep(Duration::from_millis(5));
        }
        // Let the core's bounded channel fill behind the held messages.
        std::thread::sleep(Duration::from_millis(300));
        (c.generation, start)
    }

    #[test]
    fn a_remount_during_a_held_flood_loses_and_doubles_nothing() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let first = Arc::new(Collect::default());
        let (g, _) = held_flood(&t, &first);
        let delivered: Vec<u8> = lock(&first.seen).0.concat();
        let second = Arc::new(Collect::default());
        let again = t
            .connect("main", target("run-1"), 80, 24, second.clone())
            .unwrap();
        assert!(again.reused);
        let replay = lock(&second.seen).0[0].clone();
        assert!(
            delivered.ends_with(&replay),
            "the redraw is a suffix of what the first view received"
        );
        until_with_timeout(&t, g, &second, Duration::from_secs(30), |s| {
            s.text().contains("done")
        });
        let mut all = delivered[..delivered.len() - replay.len()].to_vec();
        all.extend(lock(&second.seen).0.concat());
        let text = String::from_utf8_lossy(&all).into_owned();
        let flood = text.split_once("}}\r\n").unwrap().1;
        assert_eq!(
            flood.matches('x').count(),
            3_000_000,
            "none twice, none missing"
        );
        t.disconnect("main");
    }

    #[test]
    fn nothing_reaches_a_view_after_its_disconnect() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let _ = held_flood(&t, &sink);
        let at = sink.outputs();
        let started = Instant::now();
        assert!(t.disconnect("main").is_some());
        assert!(
            started.elapsed() < Duration::from_millis(900),
            "no wait for acknowledgements"
        );
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(sink.outputs(), at, "no output after the disconnect");
        assert!(sink.messages().is_empty(), "and no finish");
    }

    #[test]
    fn a_reconnect_on_the_same_sink_never_hears_the_old_attach() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let _ = held_flood(&t, &sink);
        t.disconnect("main");
        let at = sink.outputs();
        let c = t
            .connect("main", target("run-1"), 80, 24, sink.clone())
            .unwrap();
        assert!(!c.reused);
        std::thread::sleep(Duration::from_millis(2500));
        let after = lock(&sink.seen).0[at..].to_vec();
        assert!(
            after.iter().all(|m| !m.iter().all(|&b| b == b'x')),
            "no message of the old attach follows"
        );
        t.disconnect("main");
    }

    #[test]
    fn a_reused_connect_applies_its_size() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let first = Arc::new(Collect::default());
        let c = t
            .connect("main", target("run-1"), 80, 24, first.clone())
            .unwrap();
        hello(&t, c.generation, &first);
        let second = Arc::new(Collect::default());
        let again = t
            .connect("main", target("run-1"), 100, 30, second.clone())
            .unwrap();
        assert!(again.reused);
        assert_eq!(t.resizes("main"), 1);
        t.connect("main", target("run-1"), 100, 30, second.clone())
            .unwrap();
        assert_eq!(t.resizes("main"), 1, "the same size is not applied twice");
        t.write("main", c.generation, b"size\r").unwrap();
        until(&t, c.generation, &second, |s| {
            s.text().contains("size 100 30")
        });
        t.disconnect("main");
    }

    #[test]
    fn an_unacknowledged_view_gets_the_next_message_after_about_a_second() {
        let _tree = crate::PROCESS_TREE.read().unwrap();
        let t = terminals();
        let sink = Arc::new(Collect::default());
        let c = t
            .connect("main", target("run-1"), 80, 24, sink.clone())
            .unwrap();
        hello(&t, c.generation, &sink);
        std::thread::sleep(Duration::from_millis(50));
        ack_all(&t, c.generation, &sink);
        let start = sink.outputs();
        t.write("main", c.generation, b"flood 3000000\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while sink.outputs() < start + 2 {
            assert!(Instant::now() < deadline, "the flood starts");
            std::thread::sleep(Duration::from_millis(2));
        }
        let held = Instant::now();
        while sink.outputs() < start + 3 {
            assert!(
                held.elapsed() < Duration::from_secs(4),
                "the fallback delivers"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let waited = held.elapsed();
        assert!(waited >= Duration::from_millis(900), "waited {waited:?}");
        t.disconnect("main");
    }

    #[test]
    fn a_message_is_at_most_one_mib_and_no_byte_is_lost() {
        let pipe = Arc::new(Pipe {
            delivery: Mutex::new(Delivery {
                sink: Arc::new(Collect::default()),
                delivered: VecDeque::new(),
                unacked: 0,
                closed: false,
            }),
            acked: Condvar::new(),
        });
        let sink = Arc::new(Collect::default());
        lock(&pipe.delivery).sink = sink.clone();
        let (sender, events) = std::sync::mpsc::sync_channel(64);
        for i in 0..5u8 {
            sender
                .send(AttachEvent::Output {
                    data: vec![i; 250_000],
                })
                .unwrap();
        }
        sender.send(AttachEvent::Finished { code: 3 }).unwrap();
        let acker = {
            let pipe = Arc::clone(&pipe);
            std::thread::spawn(move || {
                let stop = Instant::now() + Duration::from_secs(5);
                while Instant::now() < stop {
                    lock(&pipe.delivery).unacked = 0;
                    pipe.acked.notify_all();
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
        };
        assert_eq!(pump(&events, &pipe), Some(3));
        let seen = lock(&sink.seen).0.clone();
        assert!(
            seen.iter().all(|m| m.len() <= 1 << 20),
            "sizes {:?}",
            seen.iter().map(Vec::len).collect::<Vec<_>>()
        );
        let all = seen.concat();
        assert_eq!(all.len(), 1_250_000);
        assert!(all
            .chunks(250_000)
            .enumerate()
            .all(|(i, c)| c.iter().all(|&b| b == i as u8)));
        drop(acker);
    }

    #[test]
    fn a_replay_without_an_explicit_hgs_refuses() {
        let t = Terminals::new(PathBuf::from("hgs"), false);
        let sink = Arc::new(Collect::default());
        assert_eq!(
            t.connect("main", target("run-1"), 80, 24, sink),
            Err("Actions in replay mode need ZERUS_CLIENT_HGS.".into())
        );
    }

    #[test]
    fn status_follows_terminal_available() {
        let input = |cmd: &str, state: &str, online: bool| StatusInput {
            facts: TerminalFacts {
                cmd: cmd.into(),
                terminal_supported: cmd != "dsh",
                ..TerminalFacts::default()
            },
            details: json!({}),
            state: state.into(),
            online,
            restored_pending: false,
            name: "claude/sample-project/review".into(),
            machine: String::new(),
        };
        let ready = status(input("claude", "running", true));
        assert!(ready.supported && ready.available && ready.reason.is_empty());
        assert_eq!(ready.idle, "Connect to use the session's live terminal");
        assert_eq!(
            ready.connecting,
            "Connecting to claude/sample-project/review…"
        );
        assert_eq!(
            ready.disconnected,
            "Disconnected — the session keeps running"
        );
        let paused = status(input("claude", "paused", true));
        assert_eq!(
            paused.reason,
            "Resume this session before opening its terminal."
        );
        let deepseek = status(input("dsh", "running", true));
        assert!(!deepseek.supported);
        assert_eq!(
            deepseek.reason,
            "This native adapter does not provide Terminal."
        );
        assert_eq!(
            status(input("claude", "running", false)).reason,
            "Machine unavailable. Reconnect when it is online."
        );
    }
}
