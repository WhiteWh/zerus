//! The menu-bar item's data and actions. The pump taps every change into a
//! thread that keeps a [`Desk`] and, at most every [`PACE`], offers the
//! status item its face, tooltip and menu on the main thread. Menu actions
//! open the window, open a running session in Terminal.app or copy its
//! command (Qt's "Open in"), open a saved session's row, or quit. A session
//! or notice the webview cannot take yet waits in [`Pending`] until it asks.
//!
//! The same thread applies this device's marks the moment they change and,
//! when this build posts them, decides the attention notifications
//! ([`Attention`]): once per batch of inputs it observes the computers that
//! changed, and every computer each [`SWEEP`], so a retry fires
//! (`TrayAgent::observeAttention`). A clicked notification opens its session
//! or reports it gone ([`DeskLoop`]).
//!
//! [`Bar`] is the status item's state apart from AppKit: what reaches it
//! ([`Gate`]), the Dock badge, the menus that are tracking, and the action
//! each item of the menu names.
// Linux shares this desk with its native SNI/DBusMenu adapter.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tauri::{AppHandle, Emitter, Manager};
use zerus_client_core::attention::{self, Changes, Host, Resolution, Tracker, CLICK_WAIT};
use zerus_client_core::marks::Mark;
use zerus_client_core::menubar::{
    tray_conf_value, Apply, Desk, DeviceMarks, Entry, Face, Gate, OpenMode,
};
use zerus_client_core::readmarks::ReadMark;
use zerus_client_core::shell;
use zerus_client_core::text::Text;
use zerus_client_core::{text, Change, ComputerId};

use crate::native_text::render;

/// The status item is offered at most this often.
pub const PACE: Duration = Duration::from_millis(250);
/// The client's own "Open in" choice, in its data directory.
pub const MODE_FILE: &str = "menubar.json";
/// At most this many notices wait for the webview.
pub const PENDING_NOTICES: usize = 8;
/// Every listed computer is observed again at least this often, so a failed
/// delivery's retry fires without a new listing.
pub const SWEEP: Duration = Duration::from_secs(10);
/// The tracker's state, in the data directory.
pub const ATTENTION_FILE: &str = "attention-observed.json";
/// Banners for optional questions (`QuestionNotificationSettings.h`).
pub const OPTIONAL_QUESTIONS: &str = "notifications/optionalQuestions";

mod attention_barrier;
pub(crate) mod observers;
pub use attention_barrier::InputSender;

pub enum DeskInput {
    Change(Box<Change>),
    Mode(OpenMode),
    /// This device's attention marks or its pending read marks changed: the
    /// desk reads them again (`FleetState::setReadState`).
    Marks,
    Updates {
        version: Option<String>,
        restart: bool,
    },
    /// A notification's default action (`MacAttentionNotifier.mm:20-28`).
    Clicked(String),
    #[cfg(target_os = "linux")]
    LinuxClicked {
        token: String,
        activation: String,
    },
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    ServiceClicked(Box<crate::desktop_service::activation::Delivery>),
    /// Public launch/instance navigation data, not callback/writer authority.
    #[cfg(target_os = "linux")]
    Navigate {
        target: zerus_client_core::desktop::activation::Target,
        activation: Option<String>,
    },
    /// A delivery failed (`AttentionNotifier::failed`).
    Failed {
        token: String,
        detail: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactSession {
    pub run: String,
    pub conversation: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenSession {
    pub computer: String,
    pub key: String,
    /// A clicked notification's session: its detail opens on Activity
    /// (`SessionsWindow::showAttentionSession`, `SessionsWindow.cpp:2837-2842`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub attention: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<ExactSession>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub title: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub open: Option<OpenSession>,
    pub notices: Vec<Notice>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub updates: bool,
}

impl Pending {
    pub fn notice(&mut self, notice: Notice) {
        if self.notices.len() == PENDING_NOTICES {
            self.notices.remove(0);
        }
        self.notices.push(notice);
    }
}

/// What the menu bar and notifications send the window's page. Until the
/// page asks ([`desk_take_pending`]) it waits in [`Pending`]; afterwards it
/// goes out as an event, until the page loads again. Each page load has a
/// number, which the page passes when it asks, so a late request from the
/// page before a reload takes nothing.
#[derive(Debug, Default)]
pub struct Inbox {
    page: u64,
    ready: bool,
    pending: Pending,
}

impl Inbox {
    /// A session to open: `send` gets it now if the page listens, else it
    /// waits. The glue calls this on the main thread and sends under the lock
    /// that [`Inbox::loading`] takes too, so a page that starts loading cannot
    /// fall between the decision and the event.
    pub fn open(&mut self, target: OpenSession, send: impl FnOnce(OpenSession)) {
        if self.ready {
            send(target);
        } else {
            self.pending.open = Some(target);
        }
    }

    pub fn open_updates(&mut self, send: impl FnOnce()) {
        if self.ready {
            send();
        } else {
            self.pending.updates = true;
        }
    }

    /// A notice: `send` gets it now if the page listens, else it waits.
    pub fn notice(&mut self, notice: Notice, send: impl FnOnce(Notice)) {
        if self.ready {
            send(notice);
        } else {
            self.pending.notice(notice);
        }
    }

    /// The page of load `page` asks for what waited and listens from now on.
    /// A request from an older page, which arrived after a reload started,
    /// takes nothing.
    pub fn take(&mut self, page: u64) -> Pending {
        if page != self.page {
            return Pending::default();
        }
        self.ready = true;
        std::mem::take(&mut self.pending)
    }

    /// A page starts loading (again): nothing listens until it asks. Returns
    /// its number.
    pub fn loading(&mut self) -> u64 {
        self.page += 1;
        self.ready = false;
        self.page
    }

    /// The number of the page loading or loaded now.
    pub fn page(&self) -> u64 {
        self.page
    }
}

/// Kept in the app state.
pub struct DeskState {
    sender: Mutex<InputSender>,
    _attention_owner: attention_barrier::Owner,
    mode: Mutex<OpenMode>,
    dir: PathBuf,
    inbox: Mutex<Inbox>,
    /// Zerus is the active app: the tracker's foreground test.
    focused: Arc<AtomicBool>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Serialize, Deserialize)]
struct Saved {
    open_mode: OpenMode,
}

/// The "Open in" choice: the client's own file, else `open-mode` from Qt's
/// `tray.conf` (read only), else Terminal.app (Decision 10).
pub fn load_mode(dir: &Path, tray_conf: Option<&str>) -> OpenMode {
    std::fs::read(dir.join(MODE_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
        .map(|saved| saved.open_mode)
        .or_else(|| {
            tray_conf
                .and_then(|text| tray_conf_value(text, "open-mode"))
                .and_then(|value| OpenMode::from_config(&value))
        })
        .unwrap_or_default()
}

/// The same configured launch terminal for New session and folder shells.
pub fn terminal_settings(dir: &Path) -> zerus_client_core::terminal_launch::Settings {
    let conf = tray_conf();
    let value = |key| {
        conf.as_deref()
            .and_then(|text| tray_conf_value(text, key))
            .unwrap_or_default()
    };
    zerus_client_core::terminal_launch::Settings {
        template: value("terminal"),
        open_mode: if cfg!(windows) {
            load_windows_mode(dir, conf.as_deref()).config().into()
        } else if cfg!(target_os = "macos") {
            serde_json::to_value(load_mode(dir, conf.as_deref()))
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default()
        } else {
            value("open-mode")
        },
    }
}

#[cfg(target_os = "linux")]
pub fn load_linux_mode(dir: &Path, tray_conf: Option<&str>) -> OpenMode {
    std::fs::read(dir.join(MODE_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
        .map(|saved| saved.open_mode)
        .filter(|mode| OpenMode::LINUX.contains(mode))
        .or_else(|| {
            tray_conf
                .and_then(|text| tray_conf_value(text, "open-mode"))
                .and_then(|value| OpenMode::from_linux_config(&value))
        })
        .unwrap_or(OpenMode::Automatic)
}

/// Windows uses Qt's auto/tab/window/console/clipboard modes.
pub fn load_windows_mode(dir: &Path, tray_conf: Option<&str>) -> OpenMode {
    std::fs::read(dir.join(MODE_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
        .map(|saved| saved.open_mode)
        .filter(|mode| OpenMode::WINDOWS.contains(mode))
        .or_else(|| {
            tray_conf
                .and_then(|text| tray_conf_value(text, "open-mode"))
                .and_then(|value| {
                    OpenMode::WINDOWS
                        .into_iter()
                        .find(|mode| mode.config() == value.trim())
                })
        })
        .unwrap_or(OpenMode::Automatic)
}

pub fn save_mode(dir: &Path, mode: OpenMode) -> std::io::Result<()> {
    write_private(
        &dir.join(MODE_FILE),
        &serde_json::to_vec(&Saved { open_mode: mode })?,
    )
}

/// Writes a private (0600) file through a temporary file and a rename.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        std::io::Write::write_all(&mut file, bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

/// `~/.config/hgs/tray.conf` (`AppConfig::configPath`), read only.
pub fn tray_conf() -> Option<String> {
    let home = std::env::var_os("HOME")?;
    std::fs::read_to_string(Path::new(&home).join(".config/hgs/tray.conf")).ok()
}

/// The changes the menu bar reads.
pub fn menu_bar_change(change: &Change) -> bool {
    matches!(
        change,
        Change::Upsert { .. }
            | Change::Remove { .. }
            | Change::Replace { .. }
            | Change::Computer { .. }
            | Change::Machine { .. }
    )
}

/// Loads the tracker on the desk thread, so its file is never read on the
/// main thread.
pub type LoadAttention = Box<dyn FnOnce() -> Attention + Send>;

/// Taps the pump, hears this device's marks change and starts the desk
/// thread. Call before the fleet starts, so the first catalog reaches the menu
/// bar; `attention` when this build posts notifications.
pub fn start(
    app: &AppHandle,
    pump: &crate::pump::Pump,
    dir: PathBuf,
    mode: OpenMode,
    attention: Option<LoadAttention>,
) {
    let (sender, inputs, owner) = attention_barrier::channel(attention.is_some());
    let attention = attention.map(|load| owner.track(load));
    let tap = sender.clone();
    pump.tap(Box::new(move |change: &Change| {
        if menu_bar_change(change) {
            let _ = tap.send(DeskInput::Change(Box::new(change.clone())));
        }
    }));
    // The marks reach the desk the moment they change, from the window's
    // commands or a settled read mark, so the menu bar follows a click in the
    // window at once.
    let marks = |sender: &InputSender| -> crate::actions::Listener {
        let sender = sender.clone();
        Box::new(move || {
            let _ = sender.send(DeskInput::Marks);
        })
    };
    if let Some(actions) = app.try_state::<crate::actions::Actions>() {
        actions.listener.set(marks(&sender));
    }
    if let Some(attention) = app.try_state::<crate::actions::Attention>() {
        attention.listener.set(marks(&sender));
    }
    let focused = Arc::new(AtomicBool::new(false));
    app.manage(DeskState {
        sender: Mutex::new(sender),
        _attention_owner: owner,
        mode: Mutex::new(mode),
        dir,
        inbox: Mutex::new(Inbox::default()),
        focused: Arc::clone(&focused),
    });
    let handle = app.clone();
    std::thread::Builder::new()
        .name("zerus-client-desk".into())
        .spawn(move || run(handle, inputs, mode, attention, focused))
        .expect("the desk thread starts");
}

/// The desk thread's input, for the notifier's clicks and failures.
pub fn sender(app: &AppHandle) -> Option<InputSender> {
    app.try_state::<DeskState>()
        .map(|state| lock(&state.sender).clone())
}

/// Whether Zerus is the active app (`Qt::ApplicationActive`): an active
/// Zerus consumes attention edges without banners.
pub fn set_focused(app: &AppHandle, focused: bool) {
    if let Some(state) = app.try_state::<DeskState>() {
        state.focused.store(focused, Ordering::Relaxed);
    }
}

/// Attention notifications, when this build posts them: the tracker and the
/// file it is kept in.
pub struct Attention {
    pub tracker: Tracker,
    pub path: PathBuf,
    checkpoint: zerus_client_core::attention_store::Observation,
    // Last: the sole source/tracker value drops before its lifetime charge.
    lifetime: Option<attention_barrier::Lifetime>,
}

impl Attention {
    /// The tracker with its saved state: the client's own file; only when
    /// there is none, Qt's `attention/observed` from `qt` (Decision 11).
    pub fn load(path: PathBuf, qt: impl FnOnce() -> Option<Vec<u8>>) -> Self {
        let bytes = std::fs::read(&path);
        let saved = match &bytes {
            Ok(bytes) => attention::seed(Some(bytes), None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                attention::seed(None, qt().as_deref())
            }
            Err(error) => {
                eprintln!("zerus-client: could not read {}: {error}", path.display());
                Map::new()
            }
        };
        let checkpoint = zerus_client_core::attention_store::Observation::loaded(&bytes, &saved);
        Self {
            tracker: Tracker::new(saved),
            path,
            checkpoint,
            lifetime: None,
        }
    }

    fn save(&mut self) {
        let serialized = serde_json::to_vec(self.tracker.state())
            .map_err(std::io::Error::other)
            .and_then(|bytes| {
                write_private(&self.path, &bytes)?;
                Ok(bytes)
            });
        if let Ok(bytes) = &serialized {
            self.checkpoint.saved(bytes);
        } else {
            self.checkpoint.failed();
        }
        if let Err(error) = serialized {
            eprintln!(
                "zerus-client: could not save {}: {error}",
                self.path.display()
            );
        }
    }

    /// Internal ordered-barrier checkpoint; no production controller calls it.
    /// Retain the actual worker through sync after an outer timeout/disconnect.
    pub(crate) fn checkpoint(
        &mut self,
    ) -> std::io::Result<zerus_client_core::attention_store::Checkpoint> {
        self.checkpoint
            .checkpoint(&self.path, self.tracker.state(), write_private)
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// One round of the tracker. Each listed computer is observed: all of them
/// with `only` None, else those in `only`. Each peer in `only` that left the
/// fleet is observed with no rows, so its banners go (`TrayAgent.cpp:460-462`).
pub fn observe_round(
    tracker: &mut Tracker,
    desk: &Desk,
    only: Option<&BTreeSet<ComputerId>>,
    now: i64,
    focused: bool,
    notice_id: &mut dyn FnMut() -> String,
) -> Changes {
    let mut all = Changes::default();
    let mut add = |changes: Changes| {
        all.raised.extend(changes.raised);
        all.cleared.extend(changes.cleared);
    };
    for (computer, state) in desk.computers() {
        if !state.listed || only.is_some_and(|only| !only.contains(computer)) {
            continue;
        }
        // A stopped stream sends `Replace(empty)` before `Ended`: a current
        // computer with no rows withdraws every banner it posted and forgets
        // them, so the next start would post them again. No code path stops
        // the source today: the fleet lives as long as the process, and a
        // removed peer's banners go anyway (`TrayAgent.cpp:460-462`).
        // Notices name a peer by its alias and this computer by its host
        // (Qt's `box.host`), empty before its card arrives.
        let label = computer
            .alias()
            .map_or_else(|| state.host.clone().unwrap_or_default(), str::to_owned);
        add(tracker.observe(
            &Host {
                alias: &computer.0,
                label: &label,
                ok: state.fresh(),
                rows: &state.rows,
            },
            now,
            focused,
            &mut *notice_id,
        ));
    }
    let empty = BTreeMap::new();
    for computer in only.into_iter().flatten() {
        if computer.alias().is_some() && !desk.computers().contains_key(computer) {
            add(tracker.observe(
                &Host {
                    alias: &computer.0,
                    label: &computer.0,
                    ok: true,
                    rows: &empty,
                },
                now,
                focused,
                &mut *notice_id,
            ));
        }
    }
    all
}

/// A clicked notification waiting for its session (`m_pendingTarget`).
struct Click {
    target: Map<String, Value>,
    deadline: Instant,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    service: Option<Box<crate::desktop_service::activation::Delivery>>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    exact: Option<zerus_client_core::desktop::activation::Target>,
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn resolve_service(
    target: &zerus_client_core::desktop::activation::Target,
    desk: &Desk,
    expired: bool,
) -> Resolution {
    let computer = ComputerId(target.computer.clone());
    let state = desk.computers().get(&computer);
    if let Some((key, _)) = state.and_then(|state| {
        state.rows.iter().find(|(_, row)| {
            row.state != "archived"
                && row.run_id.as_deref() == Some(&target.run)
                && row.conversation_id.as_deref() == Some(&target.conversation)
        })
    }) {
        return Resolution::Open {
            computer,
            key: key.clone(),
        };
    }
    if expired || state.is_some_and(|state| state.fresh()) {
        Resolution::Gone(target.run.clone())
    } else {
        Resolution::Waiting
    }
}

/// What one round reads from the app, and the status item it offers.
pub struct Context<'a> {
    /// Offers the status item its face, tooltip, menu and badge.
    pub publish: &'a mut dyn FnMut(&Desk, OpenMode),
    pub now: Instant,
    /// Seconds since the epoch, the tracker's clock.
    pub unix: i64,
    /// Zerus is the active app.
    pub focused: bool,
    /// `notifications/optionalQuestions`, read when the tracker observes.
    pub optional_questions: &'a mut dyn FnMut() -> bool,
    /// This device's marks, read when they changed.
    pub marks: &'a mut dyn FnMut() -> Option<DeviceMarks>,
    /// A new notice ID for each banner.
    pub notice_id: &'a mut dyn FnMut() -> String,
}

/// What one round asks of the app.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// The round offered the status item again.
    pub publish: bool,
    /// An "Open in" choice to save.
    pub save_mode: Option<OpenMode>,
    /// A notification was clicked: open the window (`showSessions`).
    pub show: bool,
    #[cfg(target_os = "linux")]
    pub activation: Option<String>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    completion: Option<crate::desktop_service::activation::Completion>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    expected: Option<ExactSession>,
    /// Banners to withdraw, then post.
    pub changes: Changes,
    /// The clicked notification's session.
    pub open: Option<(ComputerId, String)>,
    /// Notices for the window, kept until its page asks.
    pub notices: Vec<String>,
    /// Notices only for a window that exists (`if (m_sessionsWindow)`,
    /// `TrayAgent.cpp:200`).
    pub window_notices: Vec<String>,
}

/// The desk thread's state apart from the app: the desk, the tracker and a
/// clicked notification.
pub struct DeskLoop {
    pub desk: Desk,
    mode: OpenMode,
    pub attention: Option<Attention>,
    click: Option<Click>,
    swept: Instant,
    observing_attention: bool,
    attention_completions: bool,
}

impl DeskLoop {
    pub fn new(mode: OpenMode, attention: Option<Attention>, now: Instant) -> Self {
        Self {
            desk: Desk::default(),
            mode,
            attention,
            click: None,
            swept: now,
            observing_attention: true,
            attention_completions: true,
        }
    }

    /// How long the thread may wait for an input: until the next sweep, or
    /// until a clicked notification's wait ends.
    pub fn wait(&self, now: Instant) -> Duration {
        let mut wait = SWEEP;
        if self.attention.is_some() && self.observing_attention {
            wait = (self.swept + SWEEP).saturating_duration_since(now);
        }
        if let Some(click) = &self.click {
            wait = wait.min(click.deadline.saturating_duration_since(now));
        }
        wait
    }

    /// Start-up: the menu goes out first, with Open Zerus and Quit, before
    /// any change arrives or when the fleet does not start, as Qt builds its
    /// menu at once (`TrayAgent.cpp:252-254`); then this device's persisted
    /// marks apply, so the counts are right from the first row, before any
    /// window opens.
    pub fn start(
        &mut self,
        publish: &mut dyn FnMut(&Desk, OpenMode),
        marks: &mut dyn FnMut() -> Option<DeviceMarks>,
    ) {
        publish(&self.desk, self.mode);
        if let Some(marks) = marks() {
            self.desk.set_marks(marks);
        }
    }

    /// One batch of inputs in three steps: the changes and this device's
    /// marks; the status item, whose counts never wait for the tracker or
    /// the settings store; then one observation of the computers they
    /// touched, and the clicked session.
    pub fn round(
        &mut self,
        inputs: impl IntoIterator<Item = DeskInput>,
        cx: &mut Context,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        let mut dirty = BTreeSet::new();
        let mut marks = false;
        for input in inputs {
            match input {
                DeskInput::Change(change) => {
                    if let Some(computer) = self.desk.observe(&change) {
                        dirty.insert(computer);
                        outcome.publish = true;
                    }
                }
                DeskInput::Mode(chosen) => {
                    self.mode = chosen;
                    outcome.save_mode = Some(chosen);
                    outcome.publish = true;
                }
                DeskInput::Marks => marks = true,
                DeskInput::Updates { version, restart } => {
                    if self.desk.set_updates(version, restart) {
                        outcome.publish = true;
                    }
                }
                DeskInput::Clicked(token) if self.attention_completions => {
                    // A token that does not decode opens nothing (`TrayAgent.cpp:176-179`).
                    if let Some(target) = attention::target(&token) {
                        self.click = Some(Click {
                            target,
                            deadline: cx.now + CLICK_WAIT,
                            #[cfg(any(target_os = "linux", target_os = "macos"))]
                            service: None,
                            #[cfg(any(target_os = "linux", target_os = "macos"))]
                            exact: None,
                        });
                        outcome.show = true;
                    }
                }
                #[cfg(target_os = "linux")]
                DeskInput::LinuxClicked { token, activation } if self.attention_completions => {
                    if let Some(target) = attention::target(&token) {
                        self.click = Some(Click {
                            target,
                            deadline: cx.now + CLICK_WAIT,
                            #[cfg(target_os = "linux")]
                            service: None,
                            #[cfg(target_os = "linux")]
                            exact: None,
                        });
                        outcome.show = true;
                        outcome.activation = (!activation.is_empty()).then_some(activation);
                    }
                }
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                DeskInput::ServiceClicked(delivery)
                    if self.attention_completions && delivery.current() =>
                {
                    let open = delivery.open();
                    let target =
                        attention::target(&open.token).expect("validated activation token");
                    #[cfg(target_os = "linux")]
                    {
                        outcome.activation = open.platform_context.as_ref().map(|context| match context {
                            zerus_client_core::desktop::notifications::PlatformContext::Wayland { activation_token } => activation_token.clone(),
                        });
                    }
                    outcome.completion = Some(delivery.completion());
                    outcome.show = true;
                    self.click = Some(Click {
                        target,
                        deadline: cx.now + CLICK_WAIT,
                        exact: Some(delivery.open().target.clone()),
                        service: Some(delivery),
                    });
                }
                #[cfg(target_os = "linux")]
                DeskInput::Navigate { target, activation }
                    if self.attention_completions && target.valid() =>
                {
                    outcome.show = true;
                    outcome.activation = activation;
                    self.click = Some(Click {
                        target: Map::new(),
                        deadline: cx.now + CLICK_WAIT,
                        service: None,
                        exact: Some(target),
                    });
                }
                DeskInput::Failed { token, detail }
                    if self.attention_completions
                        || zerus_client_core::updates::is_update_token(&token) =>
                {
                    eprintln!("Zerus: notification: {detail}");
                    if self.attention_completions {
                        if let Some(attention) = self.attention.as_mut() {
                            attention.tracker.delivery_failed(&token, cx.unix);
                        }
                    }
                    outcome
                        .window_notices
                        .push(format!("System notification was not delivered: {detail}"));
                }
                DeskInput::Clicked(_) | DeskInput::Failed { .. } => {}
                #[cfg(target_os = "linux")]
                DeskInput::LinuxClicked { .. } | DeskInput::Navigate { .. } => {}
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                DeskInput::ServiceClicked(_) => {}
            }
        }
        // Read once however many changes were announced.
        if marks {
            if let Some(device) = (cx.marks)() {
                if self.desk.set_marks(device) {
                    outcome.publish = true;
                    dirty.extend(self.desk.computers().keys().cloned());
                }
            }
        }
        if outcome.publish {
            (cx.publish)(&self.desk, self.mode);
        }
        if let Some(attention) = self.attention.as_mut() {
            if self.observing_attention && cx.now >= self.swept + SWEEP {
                self.swept = cx.now;
                dirty.extend(self.desk.computers().keys().cloned());
            }
            if self.observing_attention && !dirty.is_empty() {
                // Qt reads the setting at each observation (`TrayAgent.cpp:554`).
                attention
                    .tracker
                    .set_optional_questions((cx.optional_questions)());
                outcome.changes = observe_round(
                    &mut attention.tracker,
                    &self.desk,
                    Some(&dirty),
                    cx.unix,
                    cx.focused,
                    cx.notice_id,
                );
            }
            if attention.tracker.take_changed() {
                attention.save();
            }
        }
        // A live stream is the current listing Qt asks for at the click; a
        // stale one waits for its rows up to `CLICK_WAIT` (`TrayAgent.cpp:188-192`).
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if self
            .click
            .as_ref()
            .and_then(|click| click.service.as_ref())
            .is_some_and(|delivery| !delivery.current())
        {
            self.click = None;
        }
        if let Some(click) = &self.click {
            let resolution =
                attention::resolve(&click.target, &self.desk, cx.now >= click.deadline);
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let resolution = click.exact.as_ref().map_or(resolution, |target| {
                resolve_service(target, &self.desk, cx.now >= click.deadline)
            });
            match resolution {
                Resolution::Open { computer, key } => {
                    outcome.open = Some((computer, key));
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    if let Some(delivery) = &click.service {
                        outcome.completion = Some(delivery.completion());
                    }
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    if let Some(target) = &click.exact {
                        outcome.expected = Some(ExactSession {
                            run: target.run.clone(),
                            conversation: target.conversation.clone(),
                        });
                    }
                    self.click = None;
                }
                Resolution::Gone(name) => {
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    if let Some(delivery) = &click.service {
                        outcome.completion = Some(delivery.completion());
                    }
                    outcome.notices.push(attention::gone_text(&name));
                    self.click = None;
                }
                Resolution::Waiting => {}
            }
        }
        outcome
    }
}

/// The attention marks as the desk last copied them: a change of the read
/// marks alone copies no attention map, and an unchanged map is never copied
/// under the lock the window's updates take.
#[derive(Default)]
pub struct MarksCache {
    version: Option<u64>,
    marks: BTreeMap<String, Mark>,
    replies: BTreeMap<String, String>,
}

impl MarksCache {
    /// This device's marks: `read` gives the attention maps unless their
    /// version is still the one it is passed; `pending` are the read marks
    /// waiting for their host.
    pub fn device(
        &mut self,
        read: impl FnOnce(Option<u64>) -> Option<crate::actions::MarkMaps>,
        pending: Vec<ReadMark>,
    ) -> DeviceMarks {
        if let Some((version, marks, replies)) = read(self.version) {
            self.version = Some(version);
            self.marks = marks;
            self.replies = replies;
        }
        DeviceMarks::new(self.marks.clone(), self.replies.clone(), pending)
    }
}

/// This device's marks as the overlay reads them: the attention marks and
/// read replies, and the read marks still waiting for their host.
fn device_marks(app: &AppHandle, cache: &mut MarksCache) -> Option<DeviceMarks> {
    let attention = app.try_state::<crate::actions::Attention>()?;
    let pending = app
        .try_state::<crate::actions::Actions>()
        .map(|actions| actions.pending_marks())
        .unwrap_or_default();
    Some(cache.device(|seen| attention.marks_since(seen), pending))
}

fn optional_questions(app: &AppHandle) -> bool {
    app.try_state::<crate::settings::Store>()
        .is_some_and(|store| store.0.flag(OPTIONAL_QUESTIONS))
}

fn run(
    app: AppHandle,
    inputs: Receiver<attention_barrier::Message>,
    mode: OpenMode,
    attention: Option<LoadAttention>,
    focused: Arc<AtomicBool>,
) {
    let mut state = DeskLoop::new(mode, None, Instant::now());
    let mut cache = MarksCache::default();
    state.start(&mut |desk, mode| publish(&app, desk, mode), &mut || {
        device_marks(&app, &mut cache)
    });
    state.attention = attention.map(|load| load());
    loop {
        let first = match inputs.recv_timeout(state.wait(Instant::now())) {
            Ok(input) => Some(input),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // A barrier at the head must run before any timer observation. One
        // encountered during a lazy batch runs after that batch's apply().
        let first = match first {
            Some(attention_barrier::Message::Barrier(command)) => {
                command.run(&mut state);
                continue;
            }
            other => other,
        };
        let mut batch = attention_barrier::Batch::new(first, &inputs);
        let outcome = state.round(
            &mut batch,
            &mut Context {
                publish: &mut |desk, mode| publish(&app, desk, mode),
                now: Instant::now(),
                unix: unix_now(),
                focused: focused.load(Ordering::Relaxed),
                optional_questions: &mut || optional_questions(&app),
                marks: &mut || device_marks(&app, &mut cache),
                notice_id: &mut || uuid::Uuid::new_v4().to_string(),
            },
        );
        apply(&app, outcome);
        if let Some(command) = batch.barrier {
            command.run(&mut state);
        }
        std::thread::sleep(PACE);
    }
}

/// Does what a round asked after it offered the status item, in Qt's order:
/// banners, then the window.
fn apply(app: &AppHandle, outcome: Outcome) {
    if let Some(mode) = outcome.save_mode {
        // Task 4: the desk thread saves the choice, off the main thread.
        store_mode(app, mode);
    }
    deliver(app, outcome.changes);
    if outcome.show {
        let handle = app.clone();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let completion = outcome.completion.clone();
        let _ = app.run_on_main_thread(move || {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if completion
                .as_ref()
                .is_some_and(|completion| !completion.current())
            {
                return;
            }
            #[cfg(target_os = "linux")]
            crate::platform::linux_tray::present(&handle, outcome.activation.as_deref());
            #[cfg(not(target_os = "linux"))]
            if let Err(error) = crate::lifecycle::request_main(&handle) {
                eprintln!("zerus-client: could not open the window: {error}");
            }
        });
    }
    if let Some((computer, key)) = outcome.open {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let completion = outcome.completion.clone();
        open_if(
            app,
            OpenSession {
                computer: computer.0,
                key,
                attention: true,
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                expected: outcome.expected,
                #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                expected: None,
            },
            move || {
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                if completion
                    .as_ref()
                    .is_some_and(|completion| !completion.current())
                {
                    return false;
                }
                true
            },
        );
    }
    for text in outcome.notices {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let completion = outcome.completion.clone();
        window_notice_if(app, "Zerus", &text, move || {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if completion
                .as_ref()
                .is_some_and(|completion| !completion.current())
            {
                return false;
            }
            true
        });
    }
    if crate::main_window(app).is_some() {
        for text in outcome.window_notices {
            window_notice(app, "Zerus", &text);
        }
    }
}

/// Withdraws, then posts, on the main thread (`TrayAgent.cpp:558-559`).
/// Other platforms post nothing in this phase.
fn deliver(app: &AppHandle, changes: Changes) {
    if changes.raised.is_empty() && changes.cleared.is_empty() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            crate::platform::macos_notify::deliver(&handle, changes.cleared, changes.raised)
        });
    }
    #[cfg(target_os = "linux")]
    crate::platform::linux_notify::deliver(app, changes.cleared, changes.raised);
    #[cfg(windows)]
    crate::platform::windows::tray::deliver(app, changes.cleared, changes.raised);
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    let _ = (app, changes);
}

fn publish(app: &AppHandle, desk: &Desk, mode: OpenMode) {
    #[cfg(target_os = "linux")]
    crate::platform::linux_tray::offer(
        app,
        desk.face(),
        crate::native_text::lines(&desk.tooltip()),
        desk.menu(mode),
        desk.badge(),
    );
    #[cfg(target_os = "macos")]
    {
        let (face, tooltip, menu, badge) = (
            desk.face(),
            crate::native_text::lines(&desk.tooltip()),
            desk.menu(mode),
            desk.badge(),
        );
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            crate::platform::macos_menubar::offer(&handle, face, tooltip, menu, badge)
        });
    }
    #[cfg(windows)]
    crate::platform::windows::tray::offer(
        app,
        desk.face(),
        crate::native_text::lines(&desk.tooltip()),
        desk.menu(mode),
        desk.badge(),
    );
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    let _ = (app, desk, mode);
}

/// What the webview could not take yet; its page asks once, with the number
/// [`page_loaded`] gave it.
#[tauri::command]
pub fn desk_take_pending(app: AppHandle, page: u64) -> Pending {
    app.try_state::<DeskState>()
        .map(|state| lock(&state.inbox).take(page))
        .unwrap_or_default()
}

/// The main window's page started to load (on the main thread): it gets a
/// new number, and what follows waits until it asks.
pub fn page_loading(app: &AppHandle) {
    if let Some(state) = app.try_state::<DeskState>() {
        lock(&state.inbox).loading();
    }
}

/// The main window's page has loaded (on the main thread): tells it its
/// number. Only the new document runs a script at this point, so a page
/// that is going away never learns it.
pub fn page_loaded(webview: &tauri::Webview) {
    let Some(state) = webview.app_handle().try_state::<DeskState>() else {
        return;
    };
    let page = lock(&state.inbox).page();
    let script = format!("window.__zerusDeskPage = {page}; window.__zerusDeskPageReady?.({page});");
    if let Err(error) = webview.eval(script) {
        eprintln!("zerus-client: could not tell the page its number: {error}");
    }
}

/// Opens the window on a session's row (`TrayAgent.cpp:214-216`).
pub fn open_session(app: &AppHandle, computer: &ComputerId, key: &str) {
    open(
        app,
        OpenSession {
            computer: computer.0.clone(),
            key: key.to_owned(),
            attention: false,
            expected: None,
        },
    );
}

pub fn open_updates(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let send = || {
            let _ = handle.emit("open-updates", ());
        };
        match handle.try_state::<DeskState>() {
            Some(state) => lock(&state.inbox).open_updates(send),
            None => send(),
        }
    });
}

fn open(app: &AppHandle, target: OpenSession) {
    open_if(app, target, || true);
}
fn open_if(app: &AppHandle, target: OpenSession, current: impl Fn() -> bool + Send + 'static) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if !current() {
            return;
        }
        if let Err(error) = crate::lifecycle::request_main(&handle) {
            eprintln!("zerus-client: could not open the window: {error}");
        }
        let send = |target: OpenSession| {
            let _ = handle.emit("open-session", target);
        };
        // Decided and sent under the inbox lock, on the main thread like a page load.
        match handle.try_state::<DeskState>() {
            Some(state) => lock(&state.inbox).open(target, send),
            None => send(target),
        }
        // Keep the original service delivery through the last native-main
        // action, rather than consuming its guard before window/navigation I/O.
        drop(current);
    });
}

/// A notice inside the window (Qt's `showNotificationNotice`).
pub fn window_notice(app: &AppHandle, title: &str, text: &str) {
    window_notice_if(app, title, text, || true);
}
fn window_notice_if(
    app: &AppHandle,
    title: &str,
    text: &str,
    current: impl Fn() -> bool + Send + 'static,
) {
    let notice = Notice {
        title: title.to_owned(),
        text: text.to_owned(),
    };
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if !current() {
            return;
        }
        let send = |notice: Notice| {
            let _ = handle.emit("desk-notice", notice);
        };
        // Decided and sent under the inbox lock, on the main thread like a page load.
        match handle.try_state::<DeskState>() {
            Some(state) => lock(&state.inbox).notice(notice, send),
            None => send(notice),
        }
        // Keep the original service delivery through the last native-main
        // action, rather than consuming its guard before window/navigation I/O.
        drop(current);
    });
}

/// Qt's `TrayAgent::notify` on macOS: `osascript display notification`,
/// shown at once without the app's own permission (`TrayAgent.cpp:796-830`).
pub fn tray_notify(title: &str, body: &str) {
    let spawned = crate::child_environment::command("osascript")
        .arg("-e")
        .arg(shell::notification_script(title, body))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(error) => eprintln!("zerus-client: {title}: {body} ({error})"),
    }
}

/// A running session's entry: Terminal.app or the clipboard, as "Open in"
/// says (`TrayAgent::activateSessionWith`, `TrayAgent.cpp:631-659`). A copy
/// is confirmed every time (`ClipboardBackend::open`); a failure of either
/// shows the command to run by hand.
#[cfg(target_os = "linux")]
pub use crate::platform::linux_tray::open_running;

#[cfg(windows)]
pub use crate::platform::windows::tray::open_running;

#[cfg(not(any(target_os = "linux", windows)))]
pub fn open_running(app: &AppHandle, computer: &ComputerId, name: &str) {
    let hgs = zerus_client_core::default_hgs();
    let command = shell::open_command(
        &hgs.to_string_lossy(),
        &computer.0,
        name,
        None,
        shell::current_shell(),
    );
    let mode = app
        .try_state::<DeskState>()
        .map(|state| *lock(&state.mode))
        .unwrap_or_default();
    let opened = match mode {
        OpenMode::Terminal
        | OpenMode::Automatic
        | OpenMode::Tab
        | OpenMode::Window
        | OpenMode::Console => open_in_terminal(&command),
        OpenMode::Clipboard => {
            use tauri_plugin_clipboard_manager::ClipboardExt;
            shell::copyable(&command)
                .and_then(|()| {
                    app.clipboard()
                        .write_text(command.clone())
                        .map_err(|error| {
                            format!("cannot put the command on the clipboard ({error})")
                        })
                })
                .map(|()| {
                    tray_notify(
                        "Zerus: copied to the clipboard",
                        &shell::notification_text(&command),
                    )
                })
        }
    };
    if let Err(error) = opened {
        tray_notify(
            "Zerus: the terminal did not open",
            &format!("{error}\n\nCommand: {command}\nRun it in a terminal by hand."),
        );
    }
}

/// `osascript` with Qt's Terminal.app script, detached (`MacBackend::open`).
fn open_in_terminal(command: &str) -> Result<(), String> {
    detached_script("osascript", &shell::terminal_app_script(command))
}

/// Starts `program -e script` from HOME without waiting for it. Qt reports
/// only that it could not start (`MacBackend.cpp:61-65`); the reason goes to
/// the log.
fn detached_script(program: &str, script: &str) -> Result<(), String> {
    let home = std::env::var_os("HOME").unwrap_or_else(|| "/".into());
    let spawned = crate::child_environment::command(program)
        .arg("-e")
        .arg(script)
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(())
        }
        Err(error) => {
            eprintln!("zerus-client: {program}: {error}");
            Err("cannot start osascript".to_owned())
        }
    }
}

/// Takes `chosen` unless it is the mode already in use (`TrayAgent::setOpenMode`
/// returns early on the checked entry). Returns whether it changed.
fn choose(current: &mut OpenMode, chosen: OpenMode) -> bool {
    if *current == chosen {
        return false;
    }
    *current = chosen;
    true
}

/// The "Open in" choice: applied at once (`TrayAgent::setOpenMode`). The desk
/// thread saves it, so the main thread never waits for the disk.
pub fn set_mode(app: &AppHandle, mode: OpenMode) {
    let Some(state) = app.try_state::<DeskState>() else {
        return;
    };
    if !choose(&mut lock(&state.mode), mode) {
        return;
    }
    let _ = lock(&state.sender).send(DeskInput::Mode(mode));
}

#[cfg(any(target_os = "linux", windows))]
pub fn current_mode(app: &AppHandle) -> OpenMode {
    app.try_state::<DeskState>()
        .map(|state| *lock(&state.mode))
        .unwrap_or(OpenMode::Automatic)
}

/// Saves a chosen mode; Qt's notice when it cannot (`TrayAgent.cpp:864-872`).
fn mode_saved(dir: &Path, mode: OpenMode) -> Result<(), Notice> {
    save_mode(dir, mode).map_err(|error| Notice {
        title: "Zerus: mode applied, but not saved".to_owned(),
        text: format!(
            "New sessions already open the new way, but the choice could not be written to {}: {error}",
            dir.join(MODE_FILE).display()
        ),
    })
}

/// On the desk thread: saves the mode `set_mode` applied.
fn store_mode(app: &AppHandle, mode: OpenMode) {
    let Some(dir) = app.try_state::<DeskState>().map(|state| state.dir.clone()) else {
        return;
    };
    if let Err(notice) = mode_saved(&dir, mode) {
        tray_notify(&notice.title, &notice.text);
    }
}

/// What a menu entry does when chosen (Qt's `TrayMenu` signals).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuAction {
    /// "Open Zerus" (`sessionsRequested`).
    Open,
    Updates,
    Restart,
    /// A running session opens or is copied; a saved one opens its row.
    Session {
        computer: ComputerId,
        key: String,
        name: String,
        running: bool,
        tty: String,
    },
    /// An "Open in" mode (`openModeChosen`).
    Mode(OpenMode),
    Refresh,
    Quit,
}

/// One native item of the menu, with the id its click reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// An entry that can be chosen.
    Action {
        id: String,
        text: String,
    },
    /// A line that cannot be chosen: a computer's header or its state.
    Label {
        id: String,
        text: String,
    },
    Separator,
    /// The "Open in" submenu: each mode's id, the mode and whether it is checked.
    OpenIn {
        id: String,
        modes: Vec<(String, OpenMode, bool)>,
    },
}

/// What one offer changes.
#[derive(Debug, Default, PartialEq)]
pub struct Offered {
    /// The Dock badge to set, when it changed.
    pub badge: Option<Option<String>>,
    /// The status item's changes the gate lets through.
    pub changes: Vec<Apply>,
}

/// The status item's state apart from AppKit; the glue keeps it on the main
/// thread.
#[derive(Debug, Default)]
pub struct Bar {
    gate: Gate,
    /// The Dock badge last set; None before the first offer.
    badge: Option<Option<String>>,
    /// Menus of the app that are tracking now.
    tracking: usize,
    /// Each layout's ids are new, so a click never names another entry.
    generation: u64,
    /// The actions of the latest layout.
    actions: Arc<HashMap<String, MenuAction>>,
    /// The actions of the menu shown last: a click arrives after the menu
    /// closed, when a pending change may have rebuilt it.
    shown: Arc<HashMap<String, MenuAction>>,
}

impl Bar {
    /// The desk thread's latest state. The Dock badge is not the status item:
    /// it follows attention while a menu is open, as Qt sets it before its
    /// quiet tray icon decides (`TrayAgent::refreshIcon`).
    pub fn offer(
        &mut self,
        face: Face,
        tooltip: String,
        menu: Vec<Entry>,
        badge: Option<String>,
    ) -> Offered {
        let badge = (self.badge.as_ref() != Some(&badge)).then(|| {
            self.badge = Some(badge.clone());
            badge
        });
        Offered {
            badge,
            changes: self.gate.offer(face, tooltip, menu),
        }
    }

    /// A menu of the app began tracking (`NSMenuDidBeginTrackingNotification`):
    /// nothing reaches the status item until every menu closed (`QuietTrayIcon.h`).
    pub fn began(&mut self) {
        self.tracking += 1;
        self.shown = Arc::clone(&self.actions);
        self.gate.menu_opened();
    }

    /// A menu ended tracking. True when none tracks any more: the caller then
    /// settles once the tracking loop has unwound.
    pub fn ended(&mut self) -> bool {
        self.tracking = self.tracking.saturating_sub(1);
        self.tracking == 0
    }

    /// What waited while the menus were open, unless another one opened since.
    pub fn settle(&mut self) -> Vec<Apply> {
        if self.tracking == 0 && self.gate.is_open() {
            self.gate.menu_closed()
        } else {
            Vec::new()
        }
    }

    /// No menu tracks and nothing waits for one to close: the status item may change.
    pub fn quiet(&self) -> bool {
        self.tracking == 0 && !self.gate.is_open()
    }

    /// The native items of a new menu, Qt's texts (`TrayMenu::rebuild`) in
    /// the interface language.
    pub fn layout(&mut self, entries: &[Entry]) -> Vec<Item> {
        self.layout_modes(entries, &OpenMode::ALL)
    }

    pub fn layout_modes(&mut self, entries: &[Entry], offered: &[OpenMode]) -> Vec<Item> {
        self.generation += 1;
        let mut actions = HashMap::new();
        let mut items = Vec::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            let id = format!("zerus-{}-{index}", self.generation);
            let mut chosen_by = |text: &Text, action: MenuAction| {
                actions.insert(id.clone(), action);
                Item::Action {
                    id: id.clone(),
                    text: render(text),
                }
            };
            items.push(match entry {
                Entry::Open => chosen_by(&text!("Open Zerus"), MenuAction::Open),
                Entry::Update(version) => chosen_by(
                    &text!("Update available: Zerus %1", version.as_str()),
                    MenuAction::Updates,
                ),
                Entry::Restart => {
                    chosen_by(&text!("Restart to finish update"), MenuAction::Restart)
                }
                Entry::Separator => Item::Separator,
                Entry::Label(text) => Item::Label {
                    id: id.clone(),
                    text: render(text),
                },
                Entry::Session {
                    computer,
                    key,
                    name,
                    text,
                    running,
                    tty,
                } => chosen_by(
                    text,
                    MenuAction::Session {
                        computer: computer.clone(),
                        key: key.clone(),
                        name: name.clone(),
                        running: *running,
                        tty: tty.clone(),
                    },
                ),
                Entry::OpenIn(chosen) => {
                    let modes = offered
                        .iter()
                        .copied()
                        .map(|mode| {
                            let item = format!("{id}-{}", mode.label());
                            actions.insert(item.clone(), MenuAction::Mode(mode));
                            (item, mode, mode == *chosen)
                        })
                        .collect();
                    Item::OpenIn {
                        id: id.clone(),
                        modes,
                    }
                }
                Entry::Refresh => chosen_by(&text!("Refresh"), MenuAction::Refresh),
                Entry::Quit => chosen_by(&text!("Quit"), MenuAction::Quit),
            });
        }
        self.actions = Arc::new(actions);
        items
    }

    /// The action of a clicked item, of the latest menu or the one shown.
    pub fn action(&self, id: &str) -> Option<MenuAction> {
        self.actions.get(id).or_else(|| self.shown.get(id)).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use zerus_client_core::attention::Tracker;
    use zerus_client_core::{ComputerStatus, ViewRow};

    fn asking(name: &str) -> ViewRow {
        let mut row = ViewRow {
            name: name.into(),
            state: "running".into(),
            activity: "waiting".into(),
            phase: Some("approval".into()),
            run_id: Some(format!("run-{name}")),
            ..ViewRow::default()
        };
        row.notify.needs_action = true;
        row.notify.attention_id = "r".into();
        row
    }

    fn listed(desk: &mut Desk, computer: &str, rows: Vec<ViewRow>) {
        let id = ComputerId::from(computer);
        desk.observe(&Change::Replace {
            computer: id.clone(),
            rows: rows
                .into_iter()
                .map(|row| (row.name.clone(), row))
                .collect(),
        });
        desk.observe(&Change::Computer {
            computer: id,
            status: ComputerStatus::Live,
        });
    }

    #[test]
    fn windows_terminal_modes_use_saved_choice_then_qt_config() {
        let dir = scratch("windows-modes");
        assert_eq!(load_windows_mode(&dir, None), OpenMode::Automatic);
        assert_eq!(
            load_windows_mode(&dir, Some("open-mode=console\n")),
            OpenMode::Console
        );
        save_mode(&dir, OpenMode::Tab).unwrap();
        assert_eq!(
            load_windows_mode(&dir, Some("open-mode=console\n")),
            OpenMode::Tab
        );
        save_mode(&dir, OpenMode::Terminal).unwrap();
        assert_eq!(load_windows_mode(&dir, None), OpenMode::Automatic);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_round_observes_listed_computers_and_withdraws_removed_peers() {
        let mut desk = Desk::default();
        listed(&mut desk, "", vec![asking("claude/p/here")]);
        listed(&mut desk, "mac", vec![asking("claude/p/there")]);
        desk.observe(&Change::Computer {
            computer: ComputerId::from("studio"),
            status: ComputerStatus::Probing,
        });
        let mut tracker = Tracker::new(serde_json::Map::new());
        let mut next = 0;
        let mut id = || {
            next += 1;
            format!("notice-{next}")
        };
        let first = observe_round(&mut tracker, &desk, None, 1000, false, &mut id);
        assert_eq!(
            first.raised.len(),
            2,
            "a computer that has not listed is skipped"
        );
        let mac_token = first
            .raised
            .iter()
            .find(|notice| notice.body.contains(" / mac\n"))
            .unwrap()
            .token
            .clone();
        desk.observe(&Change::Computer {
            computer: ComputerId::from("mac"),
            status: ComputerStatus::Ended,
        });
        let only: BTreeSet<ComputerId> = [ComputerId::from("mac")].into();
        let removed = observe_round(&mut tracker, &desk, Some(&only), 1001, false, &mut id);
        assert_eq!(
            removed.cleared,
            vec![mac_token],
            "a peer that left withdraws its banners"
        );
        let local_only: BTreeSet<ComputerId> = [ComputerId::local()].into();
        assert!(
            observe_round(&mut tracker, &desk, Some(&local_only), 1002, false, &mut id)
                .raised
                .is_empty()
        );
    }

    #[test]
    fn a_round_observes_only_the_computers_it_names() {
        let mut desk = Desk::default();
        listed(&mut desk, "", vec![asking("claude/p/here")]);
        listed(&mut desk, "mac", vec![asking("claude/p/there")]);
        let mut tracker = Tracker::new(serde_json::Map::new());
        let mut next = 0;
        let mut id = || {
            next += 1;
            format!("notice-{next}")
        };
        let local: BTreeSet<ComputerId> = [ComputerId::local()].into();
        let first = observe_round(&mut tracker, &desk, Some(&local), 1000, false, &mut id);
        assert_eq!(first.raised.len(), 1);
        assert!(first.raised[0].body.starts_with("claude/p/here / \n"));
        assert!(
            !tracker.state().contains_key("mac"),
            "the peer waits for its turn"
        );
        let mac: BTreeSet<ComputerId> = [ComputerId::from("mac")].into();
        let second = observe_round(&mut tracker, &desk, Some(&mac), 1001, false, &mut id);
        assert_eq!(second.raised.len(), 1);
        assert!(
            second.cleared.is_empty(),
            "a peer still listed is not taken for one that left"
        );
        // This computer is never taken for a peer that left.
        let mut fresh = Tracker::new(serde_json::Map::new());
        observe_round(
            &mut fresh,
            &Desk::default(),
            Some(&local),
            1000,
            false,
            &mut id,
        );
        assert!(fresh.state().is_empty());
        // Its notices name it by its card's host.
        desk.observe(&Change::Machine {
            computer: ComputerId::local(),
            machine: zerus_client_core::MachineMeta {
                host: Some("workstation".into()),
                ..Default::default()
            },
        });
        let mut named = Tracker::new(serde_json::Map::new());
        let raised = observe_round(&mut named, &desk, Some(&local), 1000, false, &mut id).raised;
        assert!(raised[0].body.starts_with("claude/p/here / workstation\n"));
    }

    /// A desk thread's state with a tracker in `dir`, and what its rounds read.
    struct Thread {
        state: DeskLoop,
        start: Instant,
        focused: bool,
        optional: bool,
        marks: Option<DeviceMarks>,
        marks_read: usize,
        optional_read: usize,
        notices: usize,
        /// The status item's offers (with the attention count) and the
        /// settings reads, in order.
        steps: Vec<String>,
    }

    impl Thread {
        fn new(dir: &Path) -> Self {
            let start = Instant::now();
            let attention = Attention::load(dir.join(ATTENTION_FILE), || None);
            Self {
                state: DeskLoop::new(OpenMode::Terminal, Some(attention), start),
                start,
                focused: false,
                optional: false,
                marks: None,
                marks_read: 0,
                optional_read: 0,
                notices: 0,
                steps: Vec::new(),
            }
        }

        /// One round `at` seconds after the start.
        fn round(&mut self, at: f64, inputs: Vec<DeskInput>) -> Outcome {
            let marks = self.marks.clone();
            let optional = self.optional;
            let mut read = 0;
            let mut optional_read = 0;
            let mut notices = self.notices;
            let steps = std::cell::RefCell::new(Vec::new());
            let outcome = self.state.round(
                inputs,
                &mut Context {
                    publish: &mut |desk, _| {
                        steps
                            .borrow_mut()
                            .push(format!("publish {}", desk.attention()))
                    },
                    now: self.start + Duration::from_secs_f64(at),
                    unix: 1000 + at as i64,
                    focused: self.focused,
                    optional_questions: &mut || {
                        steps.borrow_mut().push("settings".into());
                        optional_read += 1;
                        optional
                    },
                    marks: &mut || {
                        read += 1;
                        marks.clone()
                    },
                    notice_id: &mut || {
                        notices += 1;
                        format!("notice-{notices}")
                    },
                },
            );
            self.marks_read += read;
            self.optional_read += optional_read;
            self.notices = notices;
            self.steps.extend(steps.into_inner());
            outcome
        }
    }

    fn change(change: Change) -> DeskInput {
        DeskInput::Change(Box::new(change))
    }

    fn replace(computer: &str, rows: Vec<ViewRow>) -> DeskInput {
        change(Change::Replace {
            computer: ComputerId::from(computer),
            rows: rows
                .into_iter()
                .map(|row| (format!("run:{}", row.name), row))
                .collect(),
        })
    }

    fn status(computer: &str, status: ComputerStatus) -> DeskInput {
        change(Change::Computer {
            computer: ComputerId::from(computer),
            status,
        })
    }

    fn live(computer: &str, rows: Vec<ViewRow>) -> Vec<DeskInput> {
        vec![
            replace(computer, rows),
            status(computer, ComputerStatus::Live),
        ]
    }

    fn upsert(computer: &str, row: ViewRow) -> DeskInput {
        change(Change::Upsert {
            computer: ComputerId::from(computer),
            key: format!("run:{}", row.name),
            row,
        })
    }

    #[test]
    fn the_counts_go_out_before_the_tracker_reads_the_settings_store() {
        // The settings store may still be loading (Qt's seed): the face,
        // tooltip and badge never wait for it.
        let dir = scratch("publish-first");
        let mut thread = Thread::new(&dir);
        thread.round(0.0, live("", vec![asking("claude/p/a")]));
        assert_eq!(thread.steps, ["publish 1", "settings"]);
        thread.steps.clear();
        let mut row = asking("claude/p/a");
        row.attention_fingerprint = "fp-1".into();
        thread.marks = Some(acknowledged(&row));
        thread.round(1.0, vec![upsert("", row), DeskInput::Marks]);
        assert_eq!(
            thread.steps,
            ["publish 0", "settings"],
            "once per round, with the marks applied"
        );
        thread.steps.clear();
        thread.round(2.0, Vec::new());
        assert!(thread.steps.is_empty(), "nothing changed: nothing offered");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn start_up_offers_the_menu_first_then_applies_the_persisted_marks() {
        let mut row = asking("claude/p/a");
        row.attention_fingerprint = "fp-1".into();
        let marks = acknowledged(&row);
        let mut state = DeskLoop::new(OpenMode::Clipboard, None, Instant::now());
        let steps = std::cell::RefCell::new(Vec::new());
        state.start(
            &mut |desk, mode| {
                steps
                    .borrow_mut()
                    .push(format!("publish {} {:?}", desk.computers().len(), mode))
            },
            &mut || {
                steps.borrow_mut().push("marks".into());
                Some(marks.clone())
            },
        );
        assert_eq!(
            steps.into_inner(),
            ["publish 0 Clipboard", "marks"],
            "ruling 1: the empty menu goes out before anything is read"
        );
        // Ruling 2: the first row already counts this device's marks.
        let mut offered = Vec::new();
        state.round(
            live("", vec![row]),
            &mut Context {
                publish: &mut |desk, _| offered.push(desk.attention()),
                now: Instant::now(),
                unix: 1000,
                focused: false,
                optional_questions: &mut || false,
                marks: &mut || panic!("no mark changed since the start"),
                notice_id: &mut || "notice-1".into(),
            },
        );
        assert_eq!(offered, [0], "acknowledged before the window ever opened");
    }

    #[test]
    fn the_attention_maps_are_copied_only_after_they_changed() {
        let mut cache = MarksCache::default();
        let mark = |review: bool| {
            BTreeMap::from([(
                "k".to_owned(),
                Mark {
                    review,
                    acknowledged: String::new(),
                },
            )])
        };
        let pending = |run: &str| ReadMark {
            computer: "mac".into(),
            name: "claude/p/a".into(),
            run_id: run.into(),
            conversation_id: "conv-a".into(),
            reply_id: Some("r1".into()),
            attention_signature: None,
        };
        let mut asked = Vec::new();
        let first = cache.device(
            |seen| {
                asked.push(seen);
                Some((3, mark(true), BTreeMap::new()))
            },
            vec![pending("run-1")],
        );
        assert_eq!(
            first,
            DeviceMarks::new(mark(true), BTreeMap::new(), vec![pending("run-1")])
        );
        // A read mark changed alone: the attention maps are not copied again.
        let second = cache.device(
            |seen| {
                asked.push(seen);
                None
            },
            vec![pending("run-2")],
        );
        assert_eq!(
            second,
            DeviceMarks::new(mark(true), BTreeMap::new(), vec![pending("run-2")])
        );
        let third = cache.device(
            |seen| {
                asked.push(seen);
                Some((4, mark(false), BTreeMap::new()))
            },
            Vec::new(),
        );
        assert_eq!(
            third,
            DeviceMarks::new(mark(false), BTreeMap::new(), Vec::new())
        );
        assert_eq!(asked, [None, Some(3), Some(3)]);
    }

    #[test]
    fn a_batch_observes_once_and_only_the_computers_it_changed() {
        let dir = scratch("batch");
        let mut thread = Thread::new(&dir);
        let mut inputs = live("", vec![asking("claude/p/a")]);
        inputs.extend(live("mac", vec![asking("claude/p/b")]));
        let first = thread.round(0.0, inputs);
        assert_eq!(first.changes.raised.len(), 2);
        assert_eq!(
            thread.notices, 2,
            "one observation per computer for the batch"
        );
        assert!(first.publish);
        // A change on one computer observes only it.
        let mut row = asking("claude/p/c");
        row.notify.attention_id = "r2".into();
        let second = thread.round(1.0, vec![upsert("mac", row)]);
        assert_eq!(second.changes.raised.len(), 1);
        assert!(second.changes.raised[0]
            .body
            .starts_with("claude/p/c / mac\n"));
        assert!(second.changes.cleared.is_empty());
        assert_eq!(
            thread.optional_read, 2,
            "the setting is read once per observation"
        );
        // Nothing changed and no sweep is due: nothing is observed.
        let quiet = thread.round(2.0, Vec::new());
        assert_eq!(quiet, Outcome::default());
        assert_eq!(thread.optional_read, 2, "nor is the setting read");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_state_is_saved_when_it_changes_and_read_back_first() {
        let dir = scratch("saved");
        let path = dir.join(ATTENTION_FILE);
        let mut thread = Thread::new(&dir);
        thread.round(0.0, live("", vec![asking("claude/p/a")]));
        let saved = std::fs::read(&path).expect("saved after the first edge");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_file(&path).unwrap();
        thread.round(1.0, live("", vec![asking("claude/p/a")]));
        assert!(!path.exists(), "an unchanged state is not written again");
        std::fs::write(&path, &saved).unwrap();
        // A restart reads it: the same edge posts no second banner, and Qt's
        // state is not even read.
        let restarted = Attention::load(path.clone(), || panic!("Qt's state read"));
        let mut thread = DeskLoop::new(OpenMode::Terminal, Some(restarted), Instant::now());
        let outcome = thread.round(
            live("", vec![asking("claude/p/a")]),
            &mut Context {
                publish: &mut |_, _| {},
                now: Instant::now(),
                unix: 2000,
                focused: false,
                optional_questions: &mut || false,
                marks: &mut || None,
                notice_id: &mut || "notice-x".into(),
            },
        );
        assert!(outcome.changes.raised.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn attention_checkpoint_observes_the_existing_round_writer_without_rewriting_it() {
        let dir = scratch("checkpoint-round");
        let mut thread = Thread::new(&dir);
        thread.round(0.0, live("", vec![asking("claude/p/a")]));
        let path = dir.join(ATTENTION_FILE);
        let bytes = std::fs::read(&path).unwrap();
        let checkpoint = thread
            .state
            .attention
            .as_mut()
            .unwrap()
            .checkpoint()
            .unwrap();
        use sha2::{Digest, Sha256};
        assert_eq!(checkpoint.sha256, format!("{:x}", Sha256::digest(&bytes)));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn attention_checkpoint_keeps_actual_load_and_save_failures_sticky() {
        let dir = scratch("checkpoint-failure");
        let path = dir.join(ATTENTION_FILE);
        std::fs::write(&path, b"broken").unwrap();
        let mut malformed = Attention::load(path.clone(), || panic!("own source exists"));
        malformed.save(); // Old GUI behavior still writes its empty map.
        assert_eq!(std::fs::read(&path).unwrap(), b"{}");
        assert!(malformed.checkpoint().is_err());
        std::fs::remove_file(&path).unwrap();
        let mut failed = Attention::load(path.clone(), || None);
        std::fs::remove_dir(&dir).unwrap();
        failed.save(); // Existing writer cannot create its absent parent.
        std::fs::create_dir(&dir).unwrap();
        failed.save();
        assert_eq!(std::fs::read(&path).unwrap(), b"{}");
        assert!(failed.checkpoint().is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_first_start_takes_qts_state_and_a_broken_file_starts_empty() {
        let dir = scratch("seed");
        let path = dir.join(ATTENTION_FILE);
        let qt = br#"{"": {"run:run-a": {"phase": "approval", "request": "r", "token": "t"}}}"#;
        let seeded = Attention::load(path.clone(), || Some(qt.to_vec()));
        assert_eq!(
            seeded.tracker.state(),
            serde_json::from_slice::<Value>(qt)
                .unwrap()
                .as_object()
                .unwrap()
        );
        std::fs::write(&path, b"not json").unwrap();
        let broken = Attention::load(path.clone(), || Some(qt.to_vec()));
        assert!(broken.tracker.state().is_empty(), "never Qt's again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sweep_observes_every_computer_and_still_withdraws_a_peer_that_left() {
        let dir = scratch("sweep");
        let mut thread = Thread::new(&dir);
        let mut inputs = live("", vec![asking("claude/p/a")]);
        inputs.extend(live("mac", vec![asking("claude/p/b")]));
        let first = thread.round(0.0, inputs);
        let mac = first
            .changes
            .raised
            .iter()
            .find(|notice| notice.body.contains(" / mac\n"))
            .unwrap()
            .token
            .clone();
        let local = first
            .changes
            .raised
            .iter()
            .find(|notice| !notice.body.contains(" / mac\n"))
            .unwrap()
            .token
            .clone();
        assert_eq!(
            thread.state.wait(thread.start + Duration::from_secs(4)),
            Duration::from_secs(6)
        );
        // The local banner failed; its retry is due a minute later.
        thread.round(
            1.0,
            vec![DeskInput::Failed {
                token: local.clone(),
                detail: "Refused.".into(),
            }],
        );
        // The sweep comes with the peer's end in the same batch.
        let swept = thread.round(61.0, vec![status("mac", ComputerStatus::Ended)]);
        assert_eq!(
            swept.changes.cleared,
            vec![local, mac],
            "the retried banner's old token, and the peer that left"
        );
        assert_eq!(
            swept.changes.raised.len(),
            1,
            "the retry fired without a listing"
        );
        assert!(swept.changes.raised[0].body.starts_with("claude/p/a / \n"));
        assert_eq!(
            thread.state.wait(thread.start + Duration::from_secs(61)),
            SWEEP,
            "the next sweep is a full interval later"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_active_zerus_consumes_edges_and_the_setting_turns_on_optional_questions() {
        let dir = scratch("focus");
        let mut thread = Thread::new(&dir);
        thread.focused = true;
        assert!(thread
            .round(0.0, live("", vec![asking("claude/p/a")]))
            .changes
            .raised
            .is_empty());
        thread.focused = false;
        assert!(
            thread
                .round(1.0, vec![upsert("", asking("claude/p/a"))])
                .changes
                .raised
                .is_empty(),
            "a consumed edge never becomes a late banner"
        );
        let question = |ids: &[&str]| {
            let mut row = ViewRow {
                name: "claude/p/q".into(),
                state: "running".into(),
                activity: "idle".into(),
                phase: Some("idle".into()),
                run_id: Some("run-q".into()),
                conversation_id: Some("conv-q".into()),
                ..ViewRow::default()
            };
            row.notify.cmd = "claude".into();
            row.notify.optional_reported = true;
            row.notify.optional_count = ids.len() as i64;
            row.notify.optional_ids = ids.iter().map(|id| (*id).to_owned()).collect();
            row.notify.background = -1;
            row.notify.recurring_schedules = -1;
            row
        };
        thread.round(2.0, vec![upsert("", question(&["q1"]))]);
        thread.optional = true;
        let raised = thread.round(3.0, vec![upsert("", question(&["q1", "q2"]))]);
        assert_eq!(
            raised.changes.raised.len(),
            1,
            "only the question asked since"
        );
        assert_eq!(raised.changes.raised[0].title, attention::OPTIONAL_QUESTION);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn acknowledged(row: &ViewRow) -> DeviceMarks {
        let key = zerus_client_core::marks::attention_key(
            "",
            &row.agent,
            &row.name,
            row.created,
            row.conversation_id.as_deref().unwrap_or(""),
        );
        DeviceMarks::new(
            [(
                key,
                Mark {
                    review: false,
                    acknowledged: row.attention_fingerprint.clone(),
                },
            )]
            .into(),
            BTreeMap::new(),
            Vec::new(),
        )
    }

    #[test]
    fn marks_apply_at_once_read_once_per_batch_and_withdraw_the_banner() {
        let dir = scratch("marks");
        let mut thread = Thread::new(&dir);
        let mut row = asking("claude/p/a");
        row.attention_fingerprint = "fp-1".into();
        let first = thread.round(0.0, live("", vec![row.clone()]));
        assert_eq!(thread.state.desk.attention(), 1);
        let token = first.changes.raised[0].token.clone();
        assert_eq!(thread.marks_read, 0, "no mark changed");
        thread.marks = Some(acknowledged(&row));
        let marked = thread.round(1.0, vec![DeskInput::Marks, DeskInput::Marks]);
        assert_eq!(thread.marks_read, 1, "read once for the batch");
        assert!(marked.publish, "the counts follow the window's click");
        assert_eq!(thread.state.desk.attention(), 0);
        assert_eq!(
            marked.changes.cleared,
            vec![token],
            "acknowledged: the banner goes"
        );
        let same = thread.round(2.0, vec![DeskInput::Marks]);
        assert!(!same.publish, "the same marks change nothing");
        assert_eq!(same.changes, Changes::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn marks_count_while_no_tracker_runs() {
        let start = Instant::now();
        let mut state = DeskLoop::new(OpenMode::Terminal, None, start);
        let mut row = asking("claude/p/a");
        row.attention_fingerprint = "fp-1".into();
        let marks = acknowledged(&row);
        let mut cx = Context {
            publish: &mut |_, _| {},
            now: Instant::now(),
            unix: 1000,
            focused: false,
            optional_questions: &mut || panic!("no tracker reads the setting"),
            marks: &mut || Some(marks.clone()),
            notice_id: &mut || panic!("no tracker posts"),
        };
        state.round(live("", vec![row]), &mut cx);
        assert_eq!(state.desk.attention(), 1);
        assert!(state.round(vec![DeskInput::Marks], &mut cx).publish);
        assert_eq!(state.desk.attention(), 0);
        assert_eq!(
            state.wait(start + Duration::from_secs(4)),
            SWEEP,
            "nothing to sweep"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn startup_navigation_waits_for_exact_identity_and_never_creates_a_callback_receipt() {
        let dir = scratch("startup-navigation");
        let mut thread = Thread::new(&dir);
        let target = zerus_client_core::desktop::activation::Target {
            computer: "peer".into(),
            run: "original-run".into(),
            conversation: "original-conversation".into(),
        };
        let initial = thread.round(
            0.0,
            vec![DeskInput::Navigate {
                target: target.clone(),
                activation: Some("synthetic-wayland".into()),
            }],
        );
        assert!(initial.show);
        assert!(initial.open.is_none());
        assert_eq!(initial.activation.as_deref(), Some("synthetic-wayland"));
        assert!(initial.completion.is_none());
        let mut row = asking("renamed/exact-target");
        row.run_id = Some(target.run.clone());
        row.conversation_id = Some(target.conversation.clone());
        let resolved = thread.round(1.0, live("peer", vec![row]));
        assert_eq!(
            resolved.open,
            Some((ComputerId::from("peer"), "run:renamed/exact-target".into()))
        );
        assert_eq!(
            resolved.expected,
            Some(ExactSession {
                run: target.run,
                conversation: target.conversation
            })
        );
        assert!(resolved.completion.is_none());
        assert!(thread.round(2.0, Vec::new()).open.is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn service_click_requires_both_original_run_and_conversation_without_name_fallback() {
        let mut desk = Desk::default();
        let target = zerus_client_core::desktop::activation::Target {
            computer: "peer".into(),
            run: "original-run".into(),
            conversation: "original-conversation".into(),
        };
        assert_eq!(resolve_service(&target, &desk, false), Resolution::Waiting);
        let mut original = asking("codex/renamed/current");
        original.run_id = Some(target.run.clone());
        original.conversation_id = Some(target.conversation.clone());
        let mut wrong_conversation = original.clone();
        wrong_conversation.name = "same-run-other-conversation".into();
        wrong_conversation.conversation_id = Some("other-conversation".into());
        let mut wrong_run = original.clone();
        wrong_run.name = "same-conversation-other-run".into();
        wrong_run.run_id = Some("other-run".into());
        listed(&mut desk, "peer", vec![wrong_conversation, wrong_run]);
        assert!(matches!(
            resolve_service(&target, &desk, false),
            Resolution::Gone(_)
        ));
        listed(&mut desk, "other-peer", vec![original.clone()]);
        assert!(matches!(
            resolve_service(&target, &desk, false),
            Resolution::Gone(_)
        ));
        listed(&mut desk, "peer", vec![original.clone()]);
        assert_eq!(
            resolve_service(&target, &desk, false),
            Resolution::Open {
                computer: ComputerId("peer".into()),
                key: original.name.clone()
            }
        );
        original.state = "archived".into();
        listed(&mut desk, "peer", vec![original]);
        assert!(matches!(
            resolve_service(&target, &desk, false),
            Resolution::Gone(_)
        ));
    }

    #[test]
    fn a_click_with_a_live_stream_resolves_at_once() {
        let dir = scratch("click-live");
        let mut thread = Thread::new(&dir);
        let mut inputs = live("", vec![asking("claude/p/a")]);
        inputs.extend(live("mac", vec![asking("claude/p/b")]));
        let first = thread.round(0.0, inputs);
        let token = |host: &str| {
            first
                .changes
                .raised
                .iter()
                .find(|notice| notice.body.contains(&format!(" / {host}\n")))
                .unwrap()
                .token
                .clone()
        };
        let opened = thread.round(1.0, vec![DeskInput::Clicked(token("mac"))]);
        assert!(opened.show, "the window opens (`showSessions`)");
        assert_eq!(
            opened.open,
            Some((ComputerId::from("mac"), "run:claude/p/b".to_owned()))
        );
        // Gone from a live listing: reported at once, not after 16 s.
        let local = token("");
        thread.round(2.0, vec![replace("", Vec::new())]);
        let gone = thread.round(3.0, vec![DeskInput::Clicked(local)]);
        assert!(gone.show);
        assert_eq!(gone.open, None);
        assert_eq!(
            gone.notices,
            ["This session is no longer available: claude/p/a"]
        );
        assert_eq!(
            thread.round(4.0, Vec::new()).notices,
            Vec::<String>::new(),
            "once"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_linux_notification_keeps_its_activation_token_and_exact_session() {
        let dir = scratch("linux-click");
        let mut thread = Thread::new(&dir);
        let first = thread.round(0.0, live("fixture", vec![asking("claude/p/exact")]));
        let token = first.changes.raised[0].token.clone();
        let clicked = thread.round(
            1.0,
            vec![DeskInput::LinuxClicked {
                token,
                activation: "fixture-wayland-activation".into(),
            }],
        );
        assert!(clicked.show);
        assert_eq!(
            clicked.activation.as_deref(),
            Some("fixture-wayland-activation")
        );
        assert_eq!(
            clicked.open,
            Some((ComputerId::from("fixture"), "run:claude/p/exact".into()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_click_with_a_stale_stream_waits_up_to_sixteen_seconds() {
        let dir = scratch("click-stale");
        let mut thread = Thread::new(&dir);
        let first = thread.round(0.0, live("mac", vec![asking("claude/p/b")]));
        let token = first.changes.raised[0].token.clone();
        let stale = thread.round(
            1.0,
            vec![
                replace("mac", Vec::new()),
                status("mac", ComputerStatus::Reconnecting { attempt: 1 }),
            ],
        );
        assert_eq!(
            stale.changes,
            Changes::default(),
            "a stale listing keeps its banners (`AttentionTracker.cpp:55-81`)"
        );
        let waiting = thread.round(2.0, vec![DeskInput::Clicked(token.clone())]);
        assert!(waiting.show);
        assert_eq!((waiting.open, waiting.notices), (None, Vec::new()));
        assert_eq!(
            thread.state.wait(thread.start + Duration::from_secs(2)),
            Duration::from_secs(8),
            "the sweep comes first"
        );
        thread.round(10.0, Vec::new());
        assert_eq!(
            thread.state.wait(thread.start + Duration::from_secs(10)),
            Duration::from_secs(8),
            "the thread wakes when the click's wait ends"
        );
        // The stream comes back with the session: it opens.
        let back = thread.round(12.0, live("mac", vec![asking("claude/p/b")]));
        assert_eq!(
            back.open,
            Some((ComputerId::from("mac"), "run:claude/p/b".to_owned()))
        );
        // Clicked again while stale, and the session never comes back.
        let after = thread.round(
            13.0,
            vec![status("mac", ComputerStatus::Reconnecting { attempt: 1 })],
        );
        assert_eq!(after.open, None, "opened once");
        thread.round(13.5, vec![replace("mac", Vec::new())]);
        thread.round(14.0, vec![DeskInput::Clicked(token)]);
        assert_eq!(thread.round(29.9, Vec::new()).notices, Vec::<String>::new());
        assert_eq!(
            thread.round(30.0, Vec::new()).notices,
            ["This session is no longer available: claude/p/b"],
            "expired"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_token_that_does_not_decode_opens_nothing() {
        let dir = scratch("click-bad");
        let mut thread = Thread::new(&dir);
        let outcome = thread.round(0.0, vec![DeskInput::Clicked("not a token".into())]);
        assert_eq!(outcome, Outcome::default());
        assert_eq!(thread.state.wait(thread.start), SWEEP);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_delivery_is_noted_and_retried() {
        let dir = scratch("failed");
        let mut thread = Thread::new(&dir);
        let first = thread.round(0.0, live("", vec![asking("claude/p/a")]));
        let token = first.changes.raised[0].token.clone();
        std::fs::remove_file(dir.join(ATTENTION_FILE)).unwrap();
        let failed = thread.round(
            1.0,
            vec![DeskInput::Failed {
                token,
                detail: "Allow it.".into(),
            }],
        );
        assert_eq!(
            failed.window_notices,
            ["System notification was not delivered: Allow it."]
        );
        assert!(failed.notices.is_empty(), "only for a window that exists");
        assert!(
            dir.join(ATTENTION_FILE).exists(),
            "the retry time is saved at once"
        );
        assert!(thread
            .round(30.0, vec![upsert("", asking("claude/p/a"))])
            .changes
            .raised
            .is_empty());
        assert_eq!(
            thread
                .round(61.5, vec![upsert("", asking("claude/p/a"))])
                .changes
                .raised
                .len(),
            1,
            "posted again a minute later"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_attention_click_opens_on_activity_and_a_menu_row_does_not_say_so() {
        let mut open = OpenSession {
            computer: "studio".into(),
            key: "run:a".into(),
            attention: false,
            expected: None,
        };
        assert_eq!(
            serde_json::to_value(&open).unwrap(),
            serde_json::json!({"computer": "studio", "key": "run:a"})
        );
        open.attention = true;
        assert_eq!(
            serde_json::to_value(&open).unwrap(),
            serde_json::json!({"computer": "studio", "key": "run:a", "attention": true})
        );
    }

    #[test]
    fn an_exact_service_target_survives_the_cold_page_inbox_and_wire() {
        let target = OpenSession {
            computer: "peer".into(),
            key: "run:original".into(),
            attention: true,
            expected: Some(ExactSession {
                run: "original".into(),
                conversation: "conversation".into(),
            }),
        };
        let mut inbox = Inbox::default();
        inbox.open(target.clone(), |_| {
            panic!("the cold page cannot receive yet")
        });
        let pending = inbox.take(0);
        let decoded: Pending =
            serde_json::from_slice(&serde_json::to_vec(&pending).unwrap()).unwrap();
        assert_eq!(decoded.open, Some(target));
        assert!(inbox.take(0).open.is_none());
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zcd-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_open_mode_prefers_the_clients_file_then_tray_conf() {
        let dir = scratch("mode");
        assert_eq!(load_mode(&dir, None), OpenMode::Terminal);
        assert_eq!(
            load_mode(&dir, Some("open-mode=clipboard\n")),
            OpenMode::Clipboard
        );
        assert_eq!(
            load_mode(&dir, Some("open-mode=tab\n")),
            OpenMode::Terminal,
            "a Linux mode is not a macOS mode"
        );
        save_mode(&dir, OpenMode::Terminal).unwrap();
        assert_eq!(
            load_mode(&dir, Some("open-mode=clipboard\n")),
            OpenMode::Terminal
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(MODE_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pending_notices_are_bounded() {
        let mut pending = Pending::default();
        for index in 0..10 {
            pending.notice(Notice {
                title: "Zerus".into(),
                text: index.to_string(),
            });
        }
        assert_eq!(pending.notices.len(), PENDING_NOTICES);
        assert_eq!(pending.notices[0].text, "2");
    }

    #[test]
    fn the_desk_listens_to_rows_statuses_and_cards_only() {
        use zerus_client_core::{
            ActivityUpdate, ComputerStatus, MachineMeta, OrganizationUpdate, ProcessesUpdate,
            TasksUpdate, ViewRow,
        };
        let local = ComputerId::local;
        let read = [
            Change::Upsert {
                computer: local(),
                key: "k".into(),
                row: ViewRow::default(),
            },
            Change::Remove {
                computer: local(),
                key: "k".into(),
                reason: None,
            },
            Change::Replace {
                computer: local(),
                rows: Vec::new(),
            },
            Change::Computer {
                computer: local(),
                status: ComputerStatus::Live,
            },
            Change::Machine {
                computer: local(),
                machine: MachineMeta::default(),
            },
        ];
        for change in &read {
            assert!(menu_bar_change(change), "{change:?}");
        }
        let skipped = [
            Change::Activity {
                computer: local(),
                key: "k".into(),
                update: ActivityUpdate::Ended,
            },
            Change::Processes {
                computer: local(),
                key: "k".into(),
                update: ProcessesUpdate::Ended,
            },
            Change::Tasks {
                computer: local(),
                key: "k".into(),
                update: TasksUpdate::Ended,
            },
            Change::Organization {
                computer: local(),
                update: OrganizationUpdate::Ended,
            },
        ];
        for change in &skipped {
            assert!(!menu_bar_change(change), "{change:?}");
        }
    }

    #[test]
    fn a_mode_that_cannot_be_saved_gives_qts_notice() {
        let dir = scratch("unsaved");
        assert_eq!(mode_saved(&dir, OpenMode::Clipboard), Ok(()));
        assert_eq!(load_mode(&dir, None), OpenMode::Clipboard);
        // A file where the data directory should be.
        let file = dir.join("not-a-directory");
        std::fs::write(&file, b"").unwrap();
        let notice = mode_saved(&file, OpenMode::Terminal).unwrap_err();
        assert_eq!(notice.title, "Zerus: mode applied, but not saved");
        assert!(
            notice.text.starts_with(&format!(
                "New sessions already open the new way, but the choice could not be written to {}: ",
                file.join(MODE_FILE).display()
            )),
            "{}",
            notice.text
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_osascript_that_cannot_start_reads_like_qt() {
        assert_eq!(
            detached_script("/nonexistent/zerus-osascript", "return"),
            Err("cannot start osascript".to_owned())
        );
    }

    fn target(key: &str) -> OpenSession {
        OpenSession {
            computer: "studio".into(),
            key: key.into(),
            attention: false,
            expected: None,
        }
    }

    /// What an inbox call sent at once.
    fn sent<T>(call: impl FnOnce(&mut dyn FnMut(T))) -> Vec<T> {
        let mut out = Vec::new();
        call(&mut |value| out.push(value));
        out
    }

    #[test]
    fn a_session_waits_until_the_page_takes_it() {
        let mut inbox = Inbox::default();
        let page = inbox.loading();
        assert!(
            sent(|send| inbox.open(target("a"), send)).is_empty(),
            "no page yet: held"
        );
        let notice = Notice {
            title: "Zerus".into(),
            text: "sample".into(),
        };
        assert!(sent(|send| inbox.notice(notice.clone(), send)).is_empty());
        let pending = inbox.take(page);
        assert_eq!(pending.open, Some(target("a")));
        assert_eq!(pending.notices, std::slice::from_ref(&notice));
        assert_eq!(inbox.take(page), Pending::default(), "taken once");
        assert_eq!(
            sent(|send| inbox.open(target("b"), send)),
            [target("b")],
            "a ready page gets the event"
        );
        assert_eq!(sent(|send| inbox.notice(notice.clone(), send)), [notice]);
        // A window that exists but whose page loads again (a reload, a new webview).
        let next = inbox.loading();
        assert!(sent(|send| inbox.open(target("c"), send)).is_empty());
        assert!(
            sent(|send| inbox.open(target("d"), send)).is_empty(),
            "the latest session wins"
        );
        assert_eq!(inbox.take(next).open, Some(target("d")));
    }

    #[test]
    fn a_page_that_starts_loading_holds_what_follows() {
        // The decision and the send happen in one call, under the lock the page
        // load also takes, so nothing is sent to a page that started loading.
        let mut inbox = Inbox::default();
        let first = inbox.loading();
        inbox.take(first);
        inbox.loading();
        let mut sent_after_load = Vec::new();
        inbox.open(target("a"), &mut |value| sent_after_load.push(value));
        assert!(
            sent_after_load.is_empty(),
            "the reloading page would lose it"
        );
        assert_eq!(inbox.pending.open, Some(target("a")));
    }

    #[test]
    fn a_take_from_the_page_before_a_reload_is_ignored() {
        let mut inbox = Inbox::default();
        let old = inbox.loading();
        inbox.take(old);
        // The old page asks again, but its request arrives after the reload started.
        let new = inbox.loading();
        assert!(sent(|send| inbox.open(target("a"), send)).is_empty());
        assert_eq!(
            inbox.take(old),
            Pending::default(),
            "a stale take gets nothing"
        );
        assert!(
            sent(|send| inbox.open(target("b"), send)).is_empty(),
            "and does not make the new page ready"
        );
        assert_eq!(inbox.take(new).open, Some(target("b")));
        assert_eq!(inbox.page(), new);
    }

    #[test]
    fn choosing_the_mode_already_chosen_changes_nothing() {
        // `TrayAgent::setOpenMode` returns early: no new backend, no write.
        let mut mode = OpenMode::Terminal;
        assert!(!choose(&mut mode, OpenMode::Terminal));
        assert!(choose(&mut mode, OpenMode::Clipboard));
        assert_eq!(mode, OpenMode::Clipboard);
    }

    fn offer(bar: &mut Bar, total: usize, badge: Option<&str>) -> Offered {
        bar.offer(
            Face::counts(total, 0, false),
            format!("{total} sessions"),
            vec![Entry::Open, Entry::Quit],
            badge.map(str::to_owned),
        )
    }

    fn faces(changes: &[Apply]) -> Vec<Face> {
        changes
            .iter()
            .filter_map(|change| match change {
                Apply::Face(face) => Some(face.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_status_item_waits_while_any_menu_tracks() {
        let mut bar = Bar::default();
        assert_eq!(
            faces(&offer(&mut bar, 1, None).changes),
            [Face::counts(1, 0, false)]
        );
        assert!(bar.quiet());
        bar.began();
        // "Open in" or another menu of the app nests inside the first.
        bar.began();
        assert!(
            offer(&mut bar, 2, None).changes.is_empty(),
            "nothing reaches the status item while a menu tracks"
        );
        assert!(
            !bar.ended(),
            "an inner menu closed; the outer one still tracks"
        );
        assert!(offer(&mut bar, 3, None).changes.is_empty());
        assert!(bar.ended(), "the last menu closed");
        assert!(!bar.quiet(), "the status item waits for the settle");
        assert_eq!(
            faces(&bar.settle()),
            [Face::counts(3, 0, false)],
            "the latest offer shows once the menus closed"
        );
        assert!(bar.quiet());
        assert!(bar.settle().is_empty());
        assert!(!offer(&mut bar, 4, None).changes.is_empty());
    }

    #[test]
    fn a_menu_opened_before_the_close_settles_keeps_the_status_item_waiting() {
        let mut bar = Bar::default();
        offer(&mut bar, 1, None);
        bar.began();
        offer(&mut bar, 2, None);
        assert!(bar.ended());
        // The settle runs after the tracking loop unwound; a new menu came first.
        bar.began();
        assert!(bar.settle().is_empty(), "the reopened menu still tracks");
        assert!(bar.ended());
        assert_eq!(faces(&bar.settle()), [Face::counts(2, 0, false)]);
    }

    #[test]
    fn the_dock_badge_follows_attention_while_a_menu_is_open() {
        // `TrayAgent::refreshIcon` sets the badge before the quiet tray icon
        // decides; only the status item waits for the menu.
        let mut bar = Bar::default();
        assert_eq!(
            offer(&mut bar, 1, None).badge,
            Some(None),
            "the first offer sets the badge"
        );
        bar.began();
        let offered = offer(&mut bar, 2, Some("3"));
        assert_eq!(offered.badge, Some(Some("3".to_owned())));
        assert!(offered.changes.is_empty());
        assert_eq!(
            offer(&mut bar, 2, Some("3")).badge,
            None,
            "an unchanged badge is not set again"
        );
        assert_eq!(offer(&mut bar, 2, None).badge, Some(None));
    }

    fn session(key: &str, name: &str, running: bool) -> Entry {
        Entry::Session {
            computer: ComputerId::from("studio"),
            key: key.into(),
            name: name.into(),
            text: Text::verbatim(name),
            running,
            tty: String::new(),
        }
    }

    fn id(item: &Item) -> &str {
        match item {
            Item::Action { id, .. } | Item::Label { id, .. } | Item::OpenIn { id, .. } => id,
            Item::Separator => "",
        }
    }

    #[test]
    fn the_menu_follows_qt_and_each_entry_names_its_action() {
        let mut bar = Bar::default();
        let items = bar.layout(&[
            Entry::Open,
            Entry::Separator,
            Entry::Label(Text::verbatim("studio")),
            session("run:a", "claude/sample-project/a", true),
            session("run:b", "codex/sample-project/b", false),
            Entry::Separator,
            Entry::OpenIn(OpenMode::Clipboard),
            Entry::Quit,
        ]);
        let texts: Vec<&str> = items
            .iter()
            .map(|item| match item {
                Item::Action { text, .. } | Item::Label { text, .. } => text.as_str(),
                Item::OpenIn { .. } => "Open in",
                Item::Separator => "-",
            })
            .collect();
        assert_eq!(
            texts,
            [
                "Open Zerus",
                "-",
                "studio",
                "claude/sample-project/a",
                "codex/sample-project/b",
                "-",
                "Open in",
                "Quit"
            ]
        );
        assert!(
            matches!(items[2], Item::Label { .. }),
            "a header cannot be chosen"
        );
        assert_eq!(bar.action(id(&items[0])), Some(MenuAction::Open));
        assert_eq!(
            bar.action(id(&items[3])),
            Some(MenuAction::Session {
                computer: ComputerId::from("studio"),
                key: "run:a".into(),
                name: "claude/sample-project/a".into(),
                running: true,
                tty: String::new(),
            })
        );
        assert!(matches!(
            bar.action(id(&items[4])),
            Some(MenuAction::Session { running: false, .. })
        ));
        assert_eq!(bar.action(id(&items[2])), None);
        let Item::OpenIn { modes, .. } = &items[6] else {
            panic!("Open in is a submenu")
        };
        let shown: Vec<(&str, bool)> = modes
            .iter()
            .map(|(_, mode, checked)| (mode.label(), *checked))
            .collect();
        assert_eq!(
            shown,
            [("Terminal.app", false), ("Clipboard", true)],
            "Qt's macOS modes, the chosen one checked"
        );
        for (id, mode, _) in modes {
            assert_eq!(bar.action(id), Some(MenuAction::Mode(*mode)));
        }
        assert_eq!(bar.action(id(&items[7])), Some(MenuAction::Quit));
    }

    #[test]
    fn a_click_names_what_the_shown_menu_showed() {
        let mut bar = Bar::default();
        let first = bar.layout(&[
            Entry::Open,
            session("run:a", "claude/sample-project/a", true),
            Entry::Quit,
        ]);
        bar.began();
        assert!(bar.ended());
        // The menu closed with a pending change; two rebuilds may run before the click arrives.
        bar.layout(&[
            Entry::Open,
            session("run:b", "claude/sample-project/b", true),
            Entry::Quit,
        ]);
        let last = bar.layout(&[
            Entry::Quit,
            session("run:c", "claude/sample-project/c", true),
        ]);
        assert!(matches!(
            bar.action(id(&first[1])),
            Some(MenuAction::Session { key, .. }) if key == "run:a"
        ));
        assert_eq!(
            bar.action(id(&first[2])),
            Some(MenuAction::Quit),
            "never another entry's action"
        );
        assert_ne!(id(&first[0]), id(&last[0]), "a rebuilt menu has new ids");
        bar.began();
        assert_eq!(
            bar.action(id(&first[1])),
            None,
            "a menu shown since replaces the old one"
        );
    }

    #[test]
    fn a_tray_action_keeps_the_shown_tty_when_the_listing_changes() {
        let mut bar = Bar::default();
        let mut entry = session("run:a", "sh/sample-project", true);
        if let Entry::Session { computer, tty, .. } = &mut entry {
            *computer = ComputerId::local();
            *tty = "/dev/pts/7".into();
        }
        let first = bar.layout(&[entry.clone()]);
        bar.began();
        assert!(bar.ended());
        if let Entry::Session { tty, .. } = &mut entry {
            *tty = "/dev/pts/8".into();
        }
        bar.layout(&[entry]);
        assert!(
            matches!(bar.action(id(&first[0])), Some(MenuAction::Session { tty, .. }) if tty == "/dev/pts/7")
        );
        bar.began();
        assert_eq!(bar.action(id(&first[0])), None);
    }
}
