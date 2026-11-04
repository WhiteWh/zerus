//! Freedesktop attention transport, following LinuxAttentionNotifier.cpp.
//! Detection stays in the existing desk; this adapter owns only native IDs.
//! One coalesced wakeup drives bounded desired state on a worker. Withdraw
//! changes that state immediately, including while Notify waits for a reply.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::Duration;

use tauri::{AppHandle, Manager};
use zbus::blocking::{Connection, MessageIterator};
use zerus_client_core::attention::Notice;
use zerus_client_core::linux_notify::{
    decode_saved, escape_body, Namespace, Saved, FILE_LIMIT, LIMIT,
};
use zerus_freedesktop_notify::{decode_signal, Signal, PATH, SERVICE};

// Dormant resource prerequisite; the installed schema1 constructor is unchanged.
pub(crate) mod startup;

// Dormant private filesystem prerequisite; no production caller or source receipt.
#[allow(dead_code)]
mod storage;

pub const FILE: &str = "linux-notifications.json";
type Sink = Arc<dyn Fn(Event) + Send + Sync>;
type QtSeed = Box<dyn FnOnce() -> Result<Option<Saved>, String> + Send>;

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Activated { token: String, activation: String },
    Failed { token: String, detail: String },
    Uncertain(String),
}

struct Pending {
    nonce: u64,
    notice: Notice,
    sending: bool,
}
#[derive(Default)]
struct State {
    saved: Saved,
    activation: BTreeMap<u32, String>,
    pending: BTreeMap<String, Pending>,
    closes: BTreeSet<(Namespace, u32)>,
    withdrawn: BTreeSet<String>,
    nonce: u64,
    ready: bool,
    disabled: Option<String>,
}
struct Shared {
    state: Mutex<State>,
    path: PathBuf,
    sink: Sink,
    wake: mpsc::SyncSender<()>,
    stopped: AtomicBool,
    done: AtomicBool,
    connection: Mutex<Option<Connection>>,
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}
fn persist(path: &Path, saved: &Saved) -> Result<(), String> {
    let bytes = serde_json::to_vec(saved).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > FILE_LIMIT {
        return Err(
            "Notification identities exceed the storage limit; the original file was preserved"
                .into(),
        );
    }
    crate::desk::write_private(path, &bytes)
        .map_err(|e| format!("Could not save notification identities: {e}"))
}
fn load(path: &Path, qt: QtSeed) -> Result<Saved, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let saved = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => {
            if !file
                .metadata()
                .map_err(|error| error.to_string())?
                .is_file()
            {
                return Err("Notification identities must be a regular file; the original path was preserved".into());
            }
            let mut bytes = Vec::new();
            file.take(FILE_LIMIT + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() as u64 > FILE_LIMIT {
                return Err("Notification identities exceed the storage limit".into());
            }
            decode_saved(&bytes)?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => qt()?.unwrap_or(Saved {
            version: 1,
            ..Saved::default()
        }),
        Err(error) => return Err(error.to_string()),
    };
    if !saved.valid() {
        return Err(
            "Unsupported or invalid notification identities; the original file was preserved"
                .into(),
        );
    }
    Ok(saved)
}
pub fn qt_values(
) -> Result<Option<BTreeMap<String, zerus_client_core::settings::qt::QtValue>>, String> {
    zerus_client_core::settings::QtStore::here()
        .map(|store| store.read().map(|read| read.map(|(values, _)| values)))
        .unwrap_or(Ok(None))
}
fn qt_seed() -> Result<Option<Saved>, String> {
    let Some(values) = qt_values()? else {
        return Ok(None);
    };
    let owner = values
        .get("attention/linuxOwner")
        .map(|v| v.to_text())
        .unwrap_or_default();
    if owner.is_empty() {
        return Ok(None);
    }
    let tokens = values
        .get("attention/linuxTokens")
        .map(|v| v.to_text())
        .unwrap_or_default();
    let tokens = if tokens.is_empty() {
        BTreeMap::new()
    } else {
        serde_json::from_str(&tokens)
            .map_err(|e| format!("Could not read Qt notification identities: {e}"))?
    };
    Ok(Some(Saved {
        version: 1,
        bus_id: String::new(),
        owner,
        tokens,
        uncertain: BTreeSet::new(),
    }))
}

pub struct Poster {
    shared: Arc<Shared>,
}
impl Poster {
    fn spawn(path: PathBuf, qt: QtSeed, address: Option<String>, sink: Sink) -> Self {
        let (wake, receiver) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            path,
            sink,
            wake,
            stopped: AtomicBool::new(false),
            done: AtomicBool::new(false),
            connection: Mutex::new(None),
        });
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("zerus-linux-notifications".into())
            .spawn(move || {
                if let Err(error) = run(worker.clone(), receiver, qt, address.as_deref()) {
                    let tokens = {
                        let mut state = lock(&worker.state);
                        state.disabled = Some(error.clone());
                        std::mem::take(&mut state.pending)
                            .into_keys()
                            .collect::<Vec<_>>()
                    };
                    for token in tokens {
                        (worker.sink)(Event::Failed {
                            token,
                            detail: error.clone(),
                        });
                    }
                    (worker.sink)(Event::Uncertain(error));
                }
                worker.done.store(true, Ordering::Release);
            })
            .expect("notification worker starts");
        Self { shared }
    }
    pub fn deliver(&self, cleared: Vec<String>, raised: Vec<Notice>) {
        let mut state = lock(&self.shared.state);
        let mut changed = false;
        for token in cleared {
            state.pending.remove(&token);
            state.saved.uncertain.remove(&token);
            if !state.ready && state.withdrawn.len() < LIMIT {
                state.withdrawn.insert(token.clone());
            }
            let ids: Vec<_> = state
                .saved
                .tokens
                .iter()
                .filter_map(|(id, shown)| (shown == &token).then_some(*id))
                .collect();
            for id in ids {
                state.saved.tokens.remove(&id);
                state.activation.remove(&id);
                let namespace = Namespace::saved(&state.saved);
                state.closes.insert((namespace, id));
            }
            changed = true;
        }
        for notice in raised {
            if state
                .saved
                .tokens
                .values()
                .any(|token| *token == notice.token)
                || state.saved.uncertain.contains(&notice.token)
                || state.pending.contains_key(&notice.token)
            {
                continue;
            }
            let error = state.disabled.clone().or_else(|| {
                (state.saved.tokens.len()
                    + state.pending.len()
                    + state.closes.len()
                    + state.saved.uncertain.len()
                    >= LIMIT)
                    .then(|| "Too many unresolved system notifications".to_owned())
            });
            if let Some(detail) = error {
                (self.shared.sink)(Event::Failed {
                    token: notice.token,
                    detail,
                });
                continue;
            }
            state.nonce += 1;
            let nonce = state.nonce;
            state.pending.insert(
                notice.token.clone(),
                Pending {
                    nonce,
                    notice,
                    sending: false,
                },
            );
        }
        if changed && state.ready {
            if let Err(error) = persist(&self.shared.path, &state.saved) {
                (self.shared.sink)(Event::Uncertain(error));
            }
        }
        drop(state);
        let _ = self.shared.wake.try_send(());
    }
    fn stop(&self) {
        self.shared.stopped.store(true, Ordering::Relaxed);
        if let Some(connection) = lock(&self.shared.connection).take() {
            let _ = connection.close();
        }
        let _ = self.shared.wake.try_send(());
    }
}
impl Drop for Poster {
    fn drop(&mut self) {
        self.stop();
    }
}

fn owner(connection: &Connection) -> Result<String, String> {
    let bus = zbus::blocking::fdo::DBusProxy::new(connection).map_err(|error| error.to_string())?;
    match bus.get_name_owner(SERVICE.try_into().unwrap()) {
        Ok(name) => Ok(name.to_string()),
        Err(zbus::fdo::Error::NameHasNoOwner(_)) => Ok(String::new()),
        Err(error) => Err(error.to_string()),
    }
}
fn current_namespace(connection: &Connection) -> Result<Namespace, String> {
    let bus = zbus::blocking::fdo::DBusProxy::new(connection).map_err(|e| e.to_string())?;
    Ok(Namespace {
        bus_id: bus.get_id().map_err(|e| e.to_string())?.to_string(),
        owner: owner(connection)?,
    })
}
fn adopt_namespace(state: &mut State, current: &Namespace) -> bool {
    if !state.saved.adopt_namespace(current) {
        return false;
    }
    state.activation.clear();
    true
}
fn refresh_owner(shared: &Shared, connection: &Connection) -> Result<Namespace, String> {
    // Serialize the lookup and publication: an older concurrent lookup cannot
    // restore IDs from a daemon the owner-change worker already retired.
    let mut state = lock(&shared.state);
    let current = current_namespace(connection)?;
    if !adopt_namespace(&mut state, &current) {
        return Ok(current);
    }
    if state.ready {
        persist(&shared.path, &state.saved)?;
    }
    Ok(current)
}
fn close(connection: &Connection, namespace: &Namespace, id: u32) {
    if namespace.owner.is_empty()
        || namespace.bus_id.is_empty()
        || current_namespace(connection).as_ref() != Ok(namespace)
    {
        return;
    }
    if let Ok(owner) = namespace.owner.as_str().try_into() {
        let _ = zerus_freedesktop_notify::close_notification(connection, owner, id);
    }
}

fn run(
    shared: Arc<Shared>,
    receiver: mpsc::Receiver<()>,
    qt: QtSeed,
    address: Option<&str>,
) -> Result<(), String> {
    let builder = match address {
        Some(address) => zbus::blocking::connection::Builder::address(address),
        None => zbus::blocking::connection::Builder::session(),
    }
    .map_err(|e| e.to_string())?;
    let connection = builder
        .method_timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    *lock(&shared.connection) = Some(connection.clone());
    if shared.stopped.load(Ordering::Relaxed) {
        let _ = connection.close();
        return Ok(());
    }
    // Subscribe before loading IDs/looking up the owner. Both iterators have
    // bounded native queues, and each signal is verified against its owner.
    let native_rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(SERVICE)
        .unwrap()
        .path(PATH)
        .unwrap()
        .build();
    let owner_rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")
        .unwrap()
        .interface("org.freedesktop.DBus")
        .unwrap()
        .member("NameOwnerChanged")
        .unwrap()
        .add_arg(SERVICE)
        .unwrap()
        .build();
    let signals = MessageIterator::for_match_rule(native_rule, &connection, Some(256))
        .map_err(|e| e.to_string())?;
    let owners = MessageIterator::for_match_rule(owner_rule, &connection, Some(16))
        .map_err(|e| e.to_string())?;
    {
        let mut state = lock(&shared.state);
        state.saved = load(&shared.path, qt)?;
        // Quarantine legacy/different-bus IDs before processing an early
        // withdrawal, so even a pre-start clear cannot close an unproved ID.
        let current = current_namespace(&connection)?;
        adopt_namespace(&mut state, &current);
        let withdrawn = std::mem::take(&mut state.withdrawn);
        for token in withdrawn {
            state.saved.uncertain.remove(&token);
            let ids: Vec<_> = state
                .saved
                .tokens
                .iter()
                .filter_map(|(id, shown)| (shown == &token).then_some(*id))
                .collect();
            for id in ids {
                state.saved.tokens.remove(&id);
                let namespace = Namespace::saved(&state.saved);
                state.closes.insert((namespace, id));
            }
        }
        state.ready = true;
        persist(&shared.path, &state.saved)?;
    }
    let events = shared.clone();
    let signals_bus = connection.clone();
    let signals_worker = std::thread::spawn(move || {
        for message in signals {
            let Ok(message) = message else { break };
            signal(&events, &signals_bus, &message);
        }
    });
    let ownership = shared.clone();
    let owner_bus = connection.clone();
    let owner_worker = std::thread::spawn(move || {
        for message in owners {
            let Ok(message) = message else { break };
            if message
                .body()
                .deserialize::<(String, String, String)>()
                .is_ok()
            {
                // A queued older owner-change must not overwrite a newer lookup.
                if let Err(error) = refresh_owner(&ownership, &owner_bus) {
                    (ownership.sink)(Event::Uncertain(error));
                }
            }
        }
    });
    loop {
        if shared.stopped.load(Ordering::Relaxed) {
            break;
        }
        let job = {
            let mut state = lock(&shared.state);
            if let Some((owner, id)) = state.closes.pop_first() {
                Some(Work::Close(owner, id))
            } else {
                let next = state
                    .pending
                    .iter()
                    .find(|(_, pending)| !pending.sending)
                    .map(|(token, _)| token.clone());
                next.map(|token| {
                    if state.saved.tokens.values().any(|shown| shown == &token)
                        || state.saved.uncertain.contains(&token)
                    {
                        state.pending.remove(&token);
                        return Work::Continue;
                    }
                    let pending = state.pending.get_mut(&token).unwrap();
                    pending.sending = true;
                    Work::Post(pending.nonce, pending.notice.clone())
                })
            }
        };
        match job {
            Some(Work::Close(owner, id)) => close(&connection, &owner, id),
            Some(Work::Post(nonce, notice)) => post(&shared, &connection, nonce, notice),
            Some(Work::Continue) => {}
            None => {
                if receiver.recv().is_err() {
                    break;
                }
            }
        }
    }
    let _ = connection.close();
    let _ = signals_worker.join();
    let _ = owner_worker.join();
    Ok(())
}
enum Work {
    Close(Namespace, u32),
    Post(u64, Notice),
    Continue,
}
fn post(shared: &Shared, connection: &Connection, nonce: u64, notice: Notice) {
    let current = match refresh_owner(shared, connection) {
        Ok(current) => current,
        Err(detail) => {
            let mut state = lock(&shared.state);
            let wanted = state
                .pending
                .get(&notice.token)
                .is_some_and(|pending| pending.nonce == nonce);
            if wanted {
                state.pending.remove(&notice.token);
            }
            drop(state);
            if wanted {
                (shared.sink)(Event::Failed {
                    token: notice.token,
                    detail,
                });
            }
            return;
        }
    };
    {
        let mut state = lock(&shared.state);
        if !state
            .pending
            .get(&notice.token)
            .is_some_and(|pending| pending.nonce == nonce)
        {
            return;
        }
        // A notice may have queued before the saved/Qt seed loaded. Recheck
        // suppression at the effect boundary, not only in deliver().
        if state.saved.uncertain.contains(&notice.token)
            || state
                .saved
                .tokens
                .values()
                .any(|token| *token == notice.token)
        {
            state.pending.remove(&notice.token);
            return;
        }
        if current.owner.is_empty() {
            state.pending.remove(&notice.token);
            drop(state);
            (shared.sink)(Event::Failed {
                token: notice.token,
                detail: "System notification service is unavailable".into(),
            });
            return;
        }
        state.saved.uncertain.insert(notice.token.clone());
        if let Err(detail) = persist(&shared.path, &state.saved) {
            state.saved.uncertain.remove(&notice.token);
            state.pending.remove(&notice.token);
            drop(state);
            (shared.sink)(Event::Failed {
                token: notice.token,
                detail,
            });
            return;
        }
    }
    let result = (|| {
        zerus_freedesktop_notify::notify(
            connection,
            current.owner.as_str().try_into()?,
            notice.title.as_str(),
            &escape_body(&notice.body),
        )
    })();
    let mut state = lock(&shared.state);
    let wanted = state
        .pending
        .get(&notice.token)
        .is_some_and(|pending| pending.nonce == nonce);
    if wanted {
        state.pending.remove(&notice.token);
    }
    match result {
        Ok(id) if id > 0 => {
            if wanted && Namespace::saved(&state.saved) == current {
                state.saved.uncertain.remove(&notice.token);
                state.saved.tokens.insert(id, notice.token);
            } else {
                // Close only the daemon that returned this ID. Its replacement
                // may already have reused the integer for another application's card.
                drop(state);
                close(connection, &current, id);
                return;
            }
        }
        Err(zbus::Error::MethodError(name, detail, _))
            if wanted
                && !matches!(
                    name.as_str(),
                    "org.freedesktop.DBus.Error.NoReply"
                        | "org.freedesktop.DBus.Error.Timeout"
                        | "org.freedesktop.DBus.Error.TimedOut"
                        | "org.freedesktop.DBus.Error.Disconnected"
                ) =>
        {
            state.saved.uncertain.remove(&notice.token);
            (shared.sink)(Event::Failed {
                token: notice.token,
                detail: detail.unwrap_or_else(|| "System notification was refused".into()),
            });
        }
        Err(error) if wanted => (shared.sink)(Event::Uncertain(format!(
            "System notification delivery is unconfirmed: {error}"
        ))),
        Ok(_) if wanted => (shared.sink)(Event::Uncertain(
            "System notification delivery returned no identity".into(),
        )),
        _ => {}
    }
    if let Err(error) = persist(&shared.path, &state.saved) {
        (shared.sink)(Event::Uncertain(error));
    }
}
fn signal(shared: &Shared, connection: &Connection, message: &zbus::Message) {
    let header = message.header();
    if !matches!(
        header.member().map(|member| member.as_str()),
        Some("ActivationToken" | "ActionInvoked" | "NotificationClosed")
    ) {
        return;
    }
    // An owner-change signal may still be queued on its separate reader.
    // Check the current connection before interpreting a numeric ID.
    let Ok(current) = refresh_owner(shared, connection) else {
        return;
    };
    let mut state = lock(&shared.state);
    if Namespace::saved(&state.saved) != current
        || header
            .sender()
            .is_none_or(|sender| sender.as_str() != state.saved.owner)
    {
        return;
    }
    let mut event = None;
    let Some(signal) = state
        .saved
        .owner
        .as_str()
        .try_into()
        .ok()
        .and_then(|owner| decode_signal(message, &owner))
    else {
        return;
    };
    match signal {
        Signal::ActivationToken { id, token } => {
            if state.saved.tokens.contains_key(&id) {
                state.activation.insert(id, token);
            }
        }
        Signal::ActionInvoked { id, action } => {
            if action == "default" || action == "open-session" {
                if let Some(token) = state.saved.tokens.get(&id).cloned() {
                    let activation = state.activation.remove(&id).unwrap_or_default();
                    event = Some(Event::Activated { token, activation });
                }
            }
        }
        Signal::NotificationClosed { id, reason } => {
            // Plasma's expired banner remains clickable in notification history.
            if reason != 1 {
                state.saved.tokens.remove(&id);
                state.activation.remove(&id);
                if let Err(error) = persist(&shared.path, &state.saved) {
                    event = Some(Event::Uncertain(error));
                }
            }
        }
    }
    drop(state);
    if let Some(event) = event {
        (shared.sink)(event);
    }
}

pub fn install(app: &AppHandle, dir: PathBuf, switch: bool, desk: crate::desk::InputSender) {
    let target = app.clone();
    let sink = Arc::new(move |event| match event {
        Event::Activated { token, activation } => {
            if zerus_client_core::updates::is_update_token(&token) {
                if crate::updates::notification_current(&target, &token) {
                    let app = target.clone();
                    let _ = target.run_on_main_thread(move || {
                        crate::platform::linux_tray::present(&app, Some(&activation));
                        crate::updates::open_page(&app);
                    });
                }
            } else {
                let _ = desk.send(crate::desk::DeskInput::LinuxClicked { token, activation });
            }
        }
        Event::Failed { token, detail } => {
            if !zerus_client_core::updates::is_update_token(&token) {
                let _ = desk.send(crate::desk::DeskInput::Failed { token, detail });
            }
        }
        Event::Uncertain(detail) => crate::desk::window_notice(&target, "Zerus", &detail),
    });
    app.manage(Poster::spawn(
        dir.join(FILE),
        Box::new(move || if switch { qt_seed() } else { Ok(None) }),
        None,
        sink,
    ));
    // GTK's native file chooser can be active while the main Tauri window is
    // unfocused. Qt suppresses banners for the whole foreground application.
    let app = app.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(250), move || {
        use gtk::prelude::*;
        if app
            .try_state::<Poster>()
            .is_some_and(|poster| poster.shared.stopped.load(Ordering::Relaxed))
        {
            return gtk::glib::ControlFlow::Break;
        }
        crate::desk::set_focused(
            &app,
            gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Window>().ok())
                .any(|window| window.is_active()),
        );
        gtk::glib::ControlFlow::Continue
    });
}
pub fn deliver(app: &AppHandle, cleared: Vec<String>, raised: Vec<Notice>) {
    if let Some(poster) = app.try_state::<Poster>() {
        poster.deliver(cleared, raised);
    }
}
pub fn stop(app: &AppHandle) {
    if let Some(poster) = app.try_state::<Poster>() {
        poster.stop();
    }
}

#[cfg(test)]
#[path = "linux_notify_tests.rs"]
mod tests;
