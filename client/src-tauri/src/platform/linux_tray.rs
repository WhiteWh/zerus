//! KDE's native SNI and DBusMenu adapter. Tauri's AppIndicator tray does not
//! deliver Linux primary clicks. Keep the shared Desk/Bar, including its menu
//! tracking gate, and export those models directly (the approved Linux path).
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context as TaskContext, Poll, Wake, Waker};
use std::time::{Duration, Instant};

use gtk::cairo::{Context, FontSlant, FontWeight, Format, ImageSurface};
use tauri::{AppHandle, Manager};
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::export::futures_core::Stream;
use zbus::zvariant::{ObjectPath, OwnedValue, Structure, Value};
use zerus_client_core::menubar::{Apply, Entry, Face, OpenMode};
use zerus_client_core::text::Text;
use zerus_client_core::trayicon::{self, Rect, Rgb, Step};
use zerus_client_core::{text, ComputerId};

use crate::desk::{Bar, Item, MenuAction};

mod badge;
mod registration;
#[allow(unused_imports)] // Optional internal constructor has no production caller.
pub(crate) use badge::{Control as BadgeControl, Refused as BadgeRefused};

const SNI: &str = "org.kde.StatusNotifierItem";
const PATH: &str = "/StatusNotifierItem";
const MENU: &str = "/MenuBar";
const DBUSMENU: &str = "com.canonical.dbusmenu";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const BADGE: &str = "com.canonical.Unity.LauncherEntry";
const BADGE_PATH: &str = "/org/hgdev/Zerus/LauncherEntry";
const LAUNCHER: &str = "application://hgs-tray.desktop";
const SHELLS: [&str; 2] = ["com.canonical.Unity", "org.kde.plasmashell"];
type Props = HashMap<String, OwnedValue>;
type Layout = (i32, Props, Vec<OwnedValue>);
type Sink = Arc<dyn Fn(MenuAction, Option<String>) + Send + Sync>;

type Icon = (i32, i32, Vec<u8>);
struct Node {
    id: i32,
    props: Props,
    children: Vec<Node>,
}
impl Node {
    fn layout(&self, depth: i32, names: &[String]) -> Layout {
        (
            self.id,
            self.properties(names),
            if depth == 0 {
                Vec::new()
            } else {
                self.children
                    .iter()
                    .map(|node| {
                        OwnedValue::try_from(Value::from(Structure::from(
                            node.layout(depth.saturating_sub(1), names),
                        )))
                        .expect("owned menu layout")
                    })
                    .collect()
            },
        )
    }
    fn properties(&self, names: &[String]) -> Props {
        self.props
            .iter()
            .filter(|(key, _)| names.is_empty() || names.contains(key))
            .map(|(key, value)| {
                (
                    key.clone(),
                    value.try_clone().expect("menu values are scalars"),
                )
            })
            .collect()
    }
    fn find(&self, id: i32) -> Option<&Self> {
        if self.id == id {
            Some(self)
        } else {
            self.children.iter().find_map(|node| node.find(id))
        }
    }
    fn all_properties(&self, names: &[String], result: &mut Vec<(i32, Props)>) {
        result.push((self.id, self.properties(names)));
        for child in &self.children {
            child.all_properties(names, result);
        }
    }
}
fn string(value: &str) -> OwnedValue {
    OwnedValue::from(zbus::zvariant::Str::from(value))
}
fn label(text: &str) -> String {
    text.replace("&&", "&").replace('_', "__")
}

struct Tray {
    bar: Bar,
    root: Node,
    bindings: HashMap<i32, MenuAction>,
    shown: HashMap<i32, MenuAction>,
    next: i32,
    revision: u32,
    tooltip: String,
    icons: Vec<Icon>,
    foreground: Rgb,
    custom_terminal: bool,
    tracking: bool,
    activation: Option<String>,
    badge_count: i64,
    badge_sent: bool,
    badge_stopped: bool,
    sink: Sink,
}
fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|e| e.into_inner())
}
impl Tray {
    fn new(foreground: Rgb, sink: Sink) -> Self {
        Self {
            bar: Bar::default(),
            root: Node {
                id: 0,
                props: HashMap::from([("children-display".into(), string("submenu"))]),
                children: Vec::new(),
            },
            bindings: HashMap::new(),
            shown: HashMap::new(),
            next: 0,
            revision: 0,
            tooltip: "Zerus".into(),
            icons: Vec::new(),
            foreground,
            custom_terminal: false,
            tracking: false,
            activation: None,
            badge_count: 0,
            badge_sent: false,
            badge_stopped: false,
            sink,
        }
    }
    fn apply(&mut self, changes: Vec<Apply>) {
        for change in changes {
            match change {
                Apply::Face(face) => {
                    self.icons = [16, 22, 24, 32, 48]
                        .into_iter()
                        .filter_map(|size| render(&face, size, self.foreground).ok())
                        .collect()
                }
                Apply::Tooltip(text) => self.tooltip = text,
                Apply::Menu(entries) => {
                    self.bindings.clear();
                    let items = self.bar.layout_modes(&entries, &OpenMode::LINUX);
                    self.root.children = items.into_iter().map(|item| self.node(item)).collect();
                    self.revision = self.revision.wrapping_add(1);
                }
            }
        }
    }
    fn node(&mut self, item: Item) -> Node {
        // Never reuse a retired ID for a different session, even after close.
        self.next = self.next.checked_add(1).expect("DBusMenu IDs exhausted");
        let id = self.next;
        let mut props = Props::new();
        let mut children = Vec::new();
        match item {
            Item::Action { id: shared, text } => {
                props.insert("label".into(), string(&label(&text)));
                if let Some(action) = self.bar.action(&shared) {
                    self.bindings.insert(id, action);
                }
            }
            Item::Label { text, .. } => {
                props.insert("label".into(), string(&label(&text)));
                props.insert("enabled".into(), false.into());
            }
            Item::Separator => {
                props.insert("type".into(), string("separator"));
            }
            Item::OpenIn { modes, .. } => {
                props.insert(
                    "label".into(),
                    string(&crate::native_text::render(&if self.custom_terminal {
                        text!(
                            "Open in  (own terminal= is set: terminal modes are indistinguishable)"
                        )
                    } else {
                        text!("Open in")
                    })),
                );
                props.insert("children-display".into(), string("submenu"));
                for (shared, mode, checked) in modes {
                    let mut node = self.node(Item::Action {
                        id: shared,
                        text: crate::native_text::render(&Text::new(mode.label())),
                    });
                    node.props.insert("toggle-type".into(), string("radio"));
                    node.props
                        .insert("toggle-state".into(), i32::from(checked).into());
                    children.push(node);
                }
            }
        }
        Node {
            id,
            props,
            children,
        }
    }
    fn opened(&mut self) {
        if !self.tracking {
            self.shown = self.bindings.clone();
            self.tracking = true;
            self.bar.began();
        }
    }
    fn event(&mut self, id: i32, event: &str) -> bool {
        match event {
            "opened" => self.opened(),
            "closed" if id == 0 && self.tracking => {
                self.tracking = false;
                self.bar.ended();
                let changes = self.bar.settle();
                self.apply(changes);
                return true;
            }
            "clicked" => {
                if let Some(action) = self
                    .bindings
                    .get(&id)
                    .or_else(|| self.shown.get(&id))
                    .cloned()
                {
                    (self.sink)(action, self.activation.take());
                }
            }
            _ => {}
        }
        false
    }
}

fn badge_properties(count: i64) -> Props {
    HashMap::from([
        ("count".into(), count.into()),
        ("count-visible".into(), (count > 0).into()),
    ])
}
fn publish_badge(connection: &Connection, tray: &mut Tray, force: bool) {
    if tray.badge_stopped || (!force && tray.badge_sent) {
        return;
    }
    tray.badge_sent = connection
        .emit_signal(
            None::<&str>,
            BADGE_PATH,
            BADGE,
            "Update",
            &(LAUNCHER, badge_properties(tray.badge_count)),
        )
        .is_ok();
}
#[derive(Clone)]
struct BadgeService(Arc<Mutex<Tray>>);
#[zbus::interface(name = "com.canonical.Unity.LauncherEntry")]
impl BadgeService {
    #[zbus(out_args("application_uri", "properties"))]
    fn query(&self) -> (String, Props) {
        let tray = lock(&self.0);
        (LAUNCHER.into(), badge_properties(tray.badge_count))
    }
}

#[derive(Clone)]
struct ItemService(Arc<Mutex<Tray>>);
#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl ItemService {
    fn activate(&self, _x: i32, _y: i32) {
        let mut tray = lock(&self.0);
        let sink = tray.sink.clone();
        sink(MenuAction::Open, tray.activation.take());
    }
    fn secondary_activate(&self, _x: i32, _y: i32) {}
    fn context_menu(&self, _x: i32, _y: i32) {}
    fn scroll(&self, _delta: i32, _orientation: &str) {}
    fn provide_xdg_activation_token(&self, token: String) {
        lock(&self.0).activation = Some(token);
    }
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }
    #[zbus(property)]
    fn id(&self) -> &str {
        "hgs-tray"
    }
    #[zbus(property)]
    fn title(&self) -> &str {
        "Zerus"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }
    #[zbus(property)]
    fn window_id(&self) -> u32 {
        0
    }
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn menu(&self) -> ObjectPath<'static> {
        ObjectPath::try_from(MENU).unwrap()
    }
    #[zbus(property)]
    fn icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<Icon> {
        lock(&self.0).icons.clone()
    }
    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> Vec<Icon> {
        self.icon_pixmap()
    }
    #[zbus(property)]
    fn attention_icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn attention_movie_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn overlay_icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn overlay_icon_pixmap(&self) -> Vec<Icon> {
        Vec::new()
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn tool_tip(&self) -> (String, Vec<Icon>, String, String) {
        let tray = lock(&self.0);
        (
            String::new(),
            tray.icons.clone(),
            tray.tooltip.clone(),
            String::new(),
        )
    }
}
#[derive(Clone)]
struct MenuService(Arc<Mutex<Tray>>);
#[zbus::interface(name = "com.canonical.dbusmenu")]
impl MenuService {
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: Vec<String>,
    ) -> zbus::fdo::Result<(u32, Layout)> {
        let tray = lock(&self.0);
        let node = tray
            .root
            .find(parent_id)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("Unknown menu item".into()))?;
        Ok((tray.revision, node.layout(recursion_depth, &property_names)))
    }
    fn get_group_properties(&self, ids: Vec<i32>, names: Vec<String>) -> Vec<(i32, Props)> {
        let tray = lock(&self.0);
        if ids.is_empty() {
            let mut result = Vec::new();
            tray.root.all_properties(&names, &mut result);
            return result;
        }
        ids.into_iter()
            .filter_map(|id| tray.root.find(id).map(|node| (id, node.properties(&names))))
            .collect()
    }
    fn get_property(&self, id: i32, name: String) -> zbus::fdo::Result<OwnedValue> {
        let tray = lock(&self.0);
        tray.root
            .find(id)
            .and_then(|node| node.props.get(&name))
            .and_then(|value| value.try_clone().ok())
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("Unknown menu property".into()))
    }
    async fn event(
        &self,
        id: i32,
        event_id: String,
        _data: Value<'_>,
        _timestamp: u32,
        #[zbus(connection)] connection: &zbus::Connection,
    ) {
        let revision = {
            let mut tray = lock(&self.0);
            tray.event(id, &event_id).then_some(tray.revision)
        };
        if let Some(revision) = revision {
            let _ = connection
                .emit_signal(
                    None::<&str>,
                    MENU,
                    DBUSMENU,
                    "LayoutUpdated",
                    &(revision, 0_i32),
                )
                .await;
            let _ = connection
                .emit_signal(None::<&str>, PATH, SNI, "NewIcon", &())
                .await;
            let _ = connection
                .emit_signal(None::<&str>, PATH, SNI, "NewToolTip", &())
                .await;
        }
    }
    fn about_to_show(&self, _id: i32) -> bool {
        lock(&self.0).opened();
        false
    }
    async fn event_group(
        &self,
        events: Vec<(i32, String, OwnedValue, u32)>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> Vec<i32> {
        for (id, event, data, timestamp) in events {
            self.event(id, event, Value::from(data), timestamp, connection)
                .await;
        }
        Vec::new()
    }
    fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        lock(&self.0).opened();
        (Vec::new(), Vec::new())
    }
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }
    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }
}

struct Native {
    connection: Connection,
    tray: Arc<Mutex<Tray>>,
    wake: std::sync::mpsc::SyncSender<()>,
    stopped: Arc<AtomicBool>,
    badge: Option<Arc<badge::Scope>>,
}
impl Native {
    fn clear_badge(&self) {
        if let Some(scope) = &self.badge {
            let _ = scope.control().close();
        }
        self.stopped.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
        if self.badge.is_some() {
            return;
        }
        let mut tray = lock(&self.tray);
        if !tray.badge_stopped && tray.badge_count > 0 {
            tray.badge_count = 0;
            publish_badge(&self.connection, &mut tray, true);
        }
        tray.badge_stopped = true;
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        self.clear_badge();
    }
}
struct WorkerWake(std::sync::mpsc::SyncSender<()>);
impl Wake for WorkerWake {
    fn wake(self: Arc<Self>) {
        let _ = self.0.try_send(());
    }
    fn wake_by_ref(self: &Arc<Self>) {
        let _ = self.0.try_send(());
    }
}
#[cfg(test)]
fn start(foreground: Rgb, sink: Sink, address: Option<&str>) -> zbus::Result<Native> {
    start_with_badge(foreground, sink, address, None)
}
fn start_with_badge(
    foreground: Rgb,
    sink: Sink,
    address: Option<&str>,
    badge: Option<Arc<badge::Scope>>,
) -> zbus::Result<Native> {
    let tray = Arc::new(Mutex::new(Tray::new(foreground, sink)));
    let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
    let builder = match address {
        Some(address) => zbus::blocking::connection::Builder::address(address)?,
        None => zbus::blocking::connection::Builder::session()?,
    };
    let builder = builder
        .name(name.clone())?
        .method_timeout(Duration::from_secs(5))
        .serve_at(PATH, ItemService(tray.clone()))?
        .serve_at(MENU, MenuService(tray.clone()))?;
    let connection = if let Some(scope) = &badge {
        builder
            .max_queued(2)
            .serve_at(
                BADGE_PATH,
                badge::Service {
                    inner: BadgeService(tray.clone()),
                    scope: scope.clone(),
                },
            )?
            .build()?
    } else {
        builder
            .serve_at(BADGE_PATH, BadgeService(tray.clone()))?
            .build()?
    };
    // One existing worker handles SNI registration and Qt's one-second
    // coalesced launcher republish. Bounded owner streams wake it directly;
    // no second thread, polling source or unbounded event queue is added.
    let mut owners = Vec::with_capacity(3);
    for service in [WATCHER, SHELLS[0], SHELLS[1]] {
        if badge.is_some() && service != WATCHER {
            continue;
        }
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.DBus")?
            .path("/org/freedesktop/DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .add_arg(service)?
            .build();
        owners.push(MessageIterator::for_match_rule(rule, &connection, Some(2))?.into_inner());
    }
    let mut badge_streams = Vec::new();
    if let Some(scope) = &badge {
        let prepared = SHELLS
            .into_iter()
            .map(|service| badge::ShellStream::prepare(&connection, service, scope))
            .collect::<zbus::Result<Vec<_>>>()?;
        badge_streams.extend(
            prepared
                .into_iter()
                .map(|stream| stream.activate(&connection)),
        );
    }
    let bus = connection.clone();
    let state = tray.clone();
    let (wake, incoming) = std::sync::mpsc::sync_channel(1);
    let waker = Waker::from(Arc::new(WorkerWake(wake.clone())));
    let stopped = Arc::new(AtomicBool::new(false));
    let stop = stopped.clone();
    let badge_worker = badge.clone();
    if let Some(scope) = &badge {
        scope.bind_worker(wake.clone());
    }
    std::thread::spawn(move || {
        let mut registered = String::new();
        let mut watcher_revision = 0_u64;
        let mut registration: Option<(u64, registration::Task)> = None;
        let mut register_at = Some(Instant::now());
        let mut republish_at = None;
        let mut badge_closed = false;
        let mut badge_cleanup_ok = true;
        'worker: loop {
            if let Some(scope) = &badge_worker {
                if scope.closed() {
                    republish_at = None;
                    if !badge_closed {
                        badge_closed = true;
                        badge_cleanup_ok &= bus
                            .object_server()
                            .remove::<badge::Service, _>(BADGE_PATH)
                            .is_ok();
                        let removals: Vec<_> = badge_streams
                            .drain(..)
                            .map(badge::ShellStream::stop)
                            .collect();
                        for removal in removals {
                            badge_cleanup_ok &= removal.close(&bus).is_ok();
                        }
                    }
                    if let Some(fence) = scope.fence() {
                        {
                            let mut tray = lock(&state);
                            tray.badge_count = 0;
                            tray.badge_stopped = true;
                        }
                        let cleared = !scope.needs_clear()
                            || bus
                                .emit_signal(
                                    None::<&str>,
                                    BADGE_PATH,
                                    BADGE,
                                    "Update",
                                    &(LAUNCHER, badge_properties(0)),
                                )
                                .is_ok();
                        if badge_cleanup_ok && cleared {
                            fence.complete();
                        }
                        // Unknown cleanup/send leaves the admitted fence armed.
                    }
                } else if let Some((count, work)) = scope.take() {
                    let result = bus.emit_signal(
                        None::<&str>,
                        BADGE_PATH,
                        BADGE,
                        "Update",
                        &(LAUNCHER, badge_properties(count)),
                    );
                    if result.is_ok() {
                        scope.sent(count);
                        lock(&state).badge_sent = true;
                        work.complete();
                    }
                    // Failed native work is unverified and pins through Drop.
                }
                if stop.load(Ordering::Acquire) && scope.finished() {
                    break;
                }
            } else if stop.load(Ordering::Acquire) {
                break;
            }
            let mut context = TaskContext::from_waker(&waker);
            for stream in &mut badge_streams {
                for index in 0..8 {
                    match stream.poll(&mut context) {
                        Poll::Ready(Some(Ok(message))) => {
                            if stream.matches(&message) {
                                if let Ok((_, _, owner)) =
                                    message.body().deserialize::<(String, String, String)>()
                                {
                                    if !owner.is_empty() {
                                        republish_at =
                                            Some(Instant::now() + Duration::from_secs(1));
                                    }
                                }
                            }
                        }
                        Poll::Ready(_) => {
                            let _ = badge_worker.as_ref().unwrap().control().close();
                            break;
                        }
                        Poll::Pending => break,
                    }
                    if index == 7 {
                        waker.wake_by_ref();
                    }
                }
            }
            for stream in &mut owners {
                for index in 0..8 {
                    match Pin::new(&mut *stream).poll_next(&mut context) {
                        Poll::Ready(Some(Ok(message))) => {
                            if message.header().sender().map(|s| s.as_str())
                                != Some("org.freedesktop.DBus")
                            {
                                continue;
                            }
                            let Ok((service, _, owner)) =
                                message.body().deserialize::<(String, String, String)>()
                            else {
                                continue;
                            };
                            if service == WATCHER {
                                registered.clear();
                                watcher_revision = watcher_revision.wrapping_add(1);
                                register_at = (!owner.is_empty() && registration.is_none())
                                    .then(Instant::now);
                            } else if SHELLS.contains(&service.as_str()) && !owner.is_empty() {
                                republish_at = Some(Instant::now() + Duration::from_secs(1));
                            }
                        }
                        Poll::Pending => break,
                        Poll::Ready(_) => break 'worker,
                    }
                    // Preserve bounded work if a service churns continuously.
                    if index == 7 {
                        waker.wake_by_ref();
                    }
                }
            }
            if badge_worker.is_some() {
                if registration.is_none() && register_at.is_some_and(|at| Instant::now() >= at) {
                    register_at = None;
                    registration = Some((
                        watcher_revision,
                        registration::start(bus.inner().clone(), name.clone(), registered.clone()),
                    ));
                }
                if let Some((revision, task)) = registration.as_mut() {
                    if let Poll::Ready(result) = task.as_mut().poll(&mut context) {
                        let current = *revision == watcher_revision;
                        registration = None;
                        if !current {
                            // A completion from the previous watcher must never
                            // erase the replacement watcher's registration wakeup.
                            register_at = Some(Instant::now());
                        } else {
                            match result {
                                Ok(Some(owner)) => {
                                    registered = owner;
                                    register_at = None;
                                }
                                Ok(None) => register_at = None,
                                Err(_) => {
                                    register_at = Some(Instant::now() + Duration::from_secs(5))
                                }
                            }
                        }
                    }
                }
            } else {
                if register_at.is_some_and(|at| Instant::now() >= at) {
                    register_at = Some(Instant::now() + Duration::from_secs(5));
                    if let Ok(dbus) = zbus::blocking::fdo::DBusProxy::new(&bus) {
                        match dbus.get_name_owner(WATCHER.try_into().unwrap()) {
                            Ok(owner) => {
                                let owner = owner.to_string();
                                if owner == registered
                                    || Proxy::new(
                                        &bus,
                                        owner.as_str(),
                                        "/StatusNotifierWatcher",
                                        WATCHER,
                                    )
                                    .and_then(|watcher| {
                                        watcher.call::<_, _, ()>(
                                            "RegisterStatusNotifierItem",
                                            &(name.as_str(),),
                                        )
                                    })
                                    .is_ok()
                                {
                                    registered = owner;
                                    register_at = None;
                                }
                            }
                            Err(zbus::fdo::Error::NameHasNoOwner(_)) => register_at = None,
                            Err(_) => {}
                        }
                    }
                }
            }
            if republish_at.is_some_and(|at| Instant::now() >= at) {
                if let Some(scope) = &badge_worker {
                    let _ = scope.offer(lock(&state).badge_count);
                } else {
                    publish_badge(&bus, &mut lock(&state), true);
                }
                republish_at = None;
            }
            let deadline = register_at.into_iter().chain(republish_at).min();
            match deadline {
                Some(at) => {
                    let _ = incoming.recv_timeout(at.saturating_duration_since(Instant::now()));
                }
                None => {
                    if incoming.recv().is_err() {
                        break;
                    }
                }
            }
        }
        // A premature worker exit cannot silently discard queued owner cleanup.
        let removals: Vec<_> = badge_streams
            .into_iter()
            .map(badge::ShellStream::stop)
            .collect();
        for removal in removals {
            let _ = removal.close(&bus);
        }
    });
    Ok(Native {
        connection,
        tray,
        wake,
        stopped,
        badge,
    })
}
#[derive(Default)]
struct Running {
    native: Option<Native>,
    pending: Option<(Face, String, Vec<Entry>, Option<String>)>,
    stopped: bool,
}
pub struct TrayState(Mutex<Running>);
pub fn install(app: &AppHandle, conf: Option<&str>) {
    if app.manage(TrayState(Mutex::new(Running::default()))) {
        install_worker(app, conf, None, None);
    }
}
/// Optional host hook selected instead of install(), never an additional tray.
/// It admits no current producer, service/native job or positive control IPC.
#[allow(dead_code)]
pub(crate) fn install_badge_accounted(
    app: &AppHandle,
    conf: Option<&str>,
    host: zerus_client_core::desktop::drain::Charge,
) -> Result<BadgeControl, BadgeRefused> {
    if host.role() != zerus_client_core::desktop::drain::Role::Badges {
        return Err(BadgeRefused::WrongRole);
    }
    if !app.manage(TrayState(Mutex::new(Running::default()))) {
        return Err(BadgeRefused::Duplicate);
    }
    let (scope, control, startup) = badge::Scope::new(host)?;
    install_worker(app, conf, Some(scope), Some(startup));
    Ok(control)
}
fn install_worker(
    app: &AppHandle,
    conf: Option<&str>,
    badge: Option<Arc<badge::Scope>>,
    startup: Option<crate::notifier::accounting::Completion>,
) {
    let foreground = conf
        .and_then(|c| zerus_client_core::menubar::tray_conf_value(c, "icon-color"))
        .and_then(|s| trayicon::parse_color(&s))
        .unwrap_or(Rgb::new(0xe8, 0xed, 0xf4));
    let custom_terminal = conf
        .and_then(|c| zerus_client_core::menubar::tray_conf_value(c, "terminal"))
        .is_some_and(|value| !value.is_empty());
    let app = app.clone();
    std::thread::spawn(move || {
        let target = app.clone();
        match start_with_badge(
            foreground,
            Arc::new(move |action, token| dispatch(&target, action, token)),
            None,
            badge.clone(),
        ) {
            Ok(native) => {
                lock(&native.tray).custom_terminal = custom_terminal;
                let state = app.state::<TrayState>();
                let mut state = lock(&state.0);
                if state.stopped {
                    if let Some(scope) = &badge {
                        scope.ready();
                    }
                    if let Some(startup) = startup {
                        startup.complete();
                    }
                    return;
                }
                state.native = Some(native);
                if let Some((face, tooltip, entries, badge)) = state.pending.take() {
                    offer_native(
                        state.native.as_ref().unwrap(),
                        face,
                        tooltip,
                        entries,
                        badge,
                    );
                }
                if let Some(scope) = &badge {
                    scope.ready();
                }
                if let Some(startup) = startup {
                    startup.complete();
                }
            }
            Err(error) => {
                eprintln!("zerus-client: Linux tray: {error}");
                // An unavailable bus must not leave a hidden, unreachable app.
                let target = app.clone();
                let _ = app.run_on_main_thread(move || {
                    let _ = crate::lifecycle::show_main(&target);
                });
            }
        }
    });
}
pub fn offer(
    app: &AppHandle,
    face: Face,
    tooltip: String,
    entries: Vec<Entry>,
    badge: Option<String>,
) {
    let Some(state) = app.try_state::<TrayState>() else {
        return;
    };
    let mut running = lock(&state.0);
    if running.stopped {
        return;
    }
    let Some(native) = running.native.as_ref() else {
        running.pending = Some((face, tooltip, entries, badge));
        return;
    };
    offer_native(native, face, tooltip, entries, badge);
}
fn offer_native(
    native: &Native,
    face: Face,
    tooltip: String,
    mut entries: Vec<Entry>,
    badge: Option<String>,
) {
    if let Some(index) = entries
        .iter()
        .position(|entry| matches!(entry, Entry::Quit))
    {
        entries.insert(index, Entry::Refresh);
    }
    let (changed, revision) = {
        let mut tray = lock(&native.tray);
        if tray.badge_stopped && native.badge.is_none() {
            return;
        }
        let badge = if native.badge.as_ref().is_some_and(|scope| scope.closed()) {
            None
        } else {
            badge
        };
        let offered = tray.bar.offer(face, tooltip, entries, badge);
        if let Some(count) = offered.badge {
            let count = count
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0)
                .max(0);
            if native
                .badge
                .as_ref()
                .is_none_or(|scope| scope.offer(count).is_ok())
            {
                tray.badge_count = count;
                tray.badge_sent = false;
            }
        }
        if native.badge.is_none() {
            publish_badge(&native.connection, &mut tray, false);
        }
        let changed = !offered.changes.is_empty();
        tray.apply(offered.changes);
        (changed, tray.revision)
    };
    if changed {
        for signal in ["NewIcon", "NewToolTip"] {
            let _ = native
                .connection
                .emit_signal(None::<&str>, PATH, SNI, signal, &());
        }
        let _ = native.connection.emit_signal(
            None::<&str>,
            MENU,
            DBUSMENU,
            "LayoutUpdated",
            &(revision, 0_i32),
        );
    }
}
pub fn clear_badge(app: &AppHandle) {
    if let Some(state) = app.try_state::<TrayState>() {
        let mut state = lock(&state.0);
        state.stopped = true;
        state.pending = None;
        if let Some(native) = state.native.as_ref() {
            native.clear_badge();
        }
    }
}
fn dispatch(app: &AppHandle, action: MenuAction, token: Option<String>) {
    let target = app.clone();
    let _ = app.run_on_main_thread(move || match action {
        MenuAction::Open => {
            present(&target, token.as_deref());
        }
        MenuAction::Session {
            computer,
            key,
            running: false,
            ..
        } => {
            present(&target, token.as_deref());
            crate::desk::open_session(&target, &computer, &key);
        }
        MenuAction::Session {
            computer,
            name,
            tty,
            ..
        } => {
            let target = target.clone();
            std::thread::spawn(move || crate::desk::open_running(&target, &computer, &name, &tty));
        }
        MenuAction::Mode(mode) => crate::desk::set_mode(&target, mode),
        MenuAction::Updates => {
            present(&target, token.as_deref());
            crate::updates::open_page(&target);
        }
        MenuAction::Restart => crate::updates::open_page(&target),
        MenuAction::Quit => crate::exit_flush::quit_with_flush(&target, 0),
        MenuAction::Refresh => {
            let state = target.state::<crate::commands::AppState>();
            state.refresh();
        }
    });
}
pub fn present(app: &AppHandle, token: Option<&str>) {
    use gtk::prelude::*;
    if let Ok(window) = crate::lifecycle::show_main(app) {
        if let Ok(native) = window.gtk_window() {
            if let Some(token) = token {
                native.set_startup_id(token);
            }
            native.present();
        }
    }
}
pub fn open_running(app: &AppHandle, computer: &ComputerId, name: &str, tty: &str) {
    use zerus_client_core::terminal_launch::{Launcher, Opened, Settings};
    let conf = crate::desk::tray_conf();
    let mode = app
        .try_state::<crate::desk::DeskState>()
        .map(|_| crate::desk::current_mode(app))
        .unwrap_or(OpenMode::Automatic);
    let command = zerus_client_core::shell::open_command(
        &zerus_client_core::default_hgs().to_string_lossy(),
        &computer.0,
        name,
        None,
        zerus_client_core::shell::current_shell(),
    );
    if mode == OpenMode::Clipboard {
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            match crate::clipboard::copy_text(app.clone(), command.clone()) {
                Ok(()) => crate::desk::window_notice(
                    &app,
                    "Zerus: copied to the clipboard",
                    &zerus_client_core::shell::notification_text(&command),
                ),
                Err(error) => crate::desk::window_notice(
                    &app,
                    "Zerus: the terminal did not open",
                    &zerus_client_core::terminal_launch::not_opened(&error, &command),
                ),
            }
        });
        return;
    }
    let mut launcher = Launcher::new(Settings {
        open_mode: mode.config().into(),
        template: conf
            .as_deref()
            .and_then(|c| zerus_client_core::menubar::tray_conf_value(c, "terminal"))
            .unwrap_or_default(),
    });
    match launcher.open_session(
        &zerus_client_core::default_hgs().to_string_lossy(),
        &computer.0,
        name,
        if computer == &ComputerId::local() {
            tty
        } else {
            ""
        },
    ) {
        Ok(Opened::Opened {
            advisory: Some(text),
        }) => crate::desk::window_notice(
            app,
            zerus_client_core::terminal_launch::ADVISORY_TITLE,
            &text,
        ),
        Err(error) => crate::desk::window_notice(
            app,
            zerus_client_core::terminal_launch::NOT_OPENED_TITLE,
            &zerus_client_core::terminal_launch::not_opened(&error, &command),
        ),
        _ => {}
    }
}

fn color(context: &Context, rgb: Rgb) {
    context.set_source_rgba(
        f64::from(rgb.r) / 255.,
        f64::from(rgb.g) / 255.,
        f64::from(rgb.b) / 255.,
        f64::from(rgb.a) / 255.,
    );
}
fn rounded(context: &Context, r: Rect, radius: f64) -> Result<(), gtk::cairo::Error> {
    let radius = radius.min(r.h / 2.);
    context.new_sub_path();
    for (x, y, start) in [
        (r.x + r.w - radius, r.y + radius, -90.),
        (r.x + r.w - radius, r.y + r.h - radius, 0.),
        (r.x + radius, r.y + r.h - radius, 90.),
        (r.x + radius, r.y + radius, 180.),
    ] {
        context.arc(
            x,
            y,
            radius,
            f64::to_radians(start),
            f64::to_radians(start + 90.),
        );
    }
    context.close_path();
    context.fill()
}
fn render(face: &Face, size: i32, foreground: Rgb) -> Result<Icon, Box<dyn std::error::Error>> {
    let mut surface = ImageSurface::create(Format::ARgb32, size, size)?;
    {
        let c = Context::new(&surface)?;
        let tile = trayicon::tile(face, f64::from(size), foreground);
        color(&c, tile.surface);
        rounded(&c, tile.upper, tile.radius)?;
        color(&c, tile.lower_fill);
        rounded(&c, tile.lower, tile.radius)?;
        if let Some(mark) = tile.mark {
            c.save()?;
            c.translate(mark.x, mark.y);
            c.scale(mark.w / 100., mark.h / 100.);
            for step in trayicon::mark_path() {
                match step {
                    Step::Move(x, y) => c.move_to(x, y),
                    Step::Curve { c1, c2, to } => c.curve_to(c1.0, c1.1, c2.0, c2.1, to.0, to.1),
                    Step::Close => c.close_path(),
                }
            }
            color(&c, foreground);
            c.fill()?;
            c.restore()?;
        }
        c.select_font_face("sans-serif", FontSlant::Normal, FontWeight::Bold);
        for text in [tile.total, tile.attention] {
            let mut pixels = tile.font_pixels;
            let extents = loop {
                c.set_font_size(pixels);
                let e = c.text_extents(&text.text)?;
                if e.width() <= text.rect.w || pixels <= 5. {
                    break e;
                }
                pixels -= 1.;
            };
            color(&c, text.color);
            c.move_to(
                text.rect.x + (text.rect.w - extents.width()) / 2. - extents.x_bearing(),
                text.rect.y + (text.rect.h - extents.height()) / 2. - extents.y_bearing(),
            );
            c.show_text(&text.text)?;
        }
        if let Some((x, y, r)) = tile.dot {
            color(&c, trayicon::AMBER);
            c.arc(x, y, r, 0., std::f64::consts::TAU);
            c.fill()?;
        }
    }
    surface.flush();
    let bytes = surface.data()?;
    let data = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|pixel| u32::from_ne_bytes(*pixel).to_be_bytes())
        .collect();
    Ok((size, size, data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn badge_updates(connection: &Connection, owner: &str) -> zbus::MessageStream {
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(owner)
            .unwrap()
            .path(BADGE_PATH)
            .unwrap()
            .interface(BADGE)
            .unwrap()
            .member("Update")
            .unwrap()
            .build();
        MessageIterator::for_match_rule(rule, connection, Some(2))
            .unwrap()
            .into_inner()
    }
    fn next_badge(stream: &mut zbus::MessageStream, timeout: Duration) -> Option<(String, Props)> {
        let deadline = Instant::now() + timeout;
        loop {
            match Pin::new(&mut *stream).poll_next(&mut TaskContext::from_waker(Waker::noop())) {
                Poll::Ready(Some(Ok(message))) => {
                    return Some(message.body().deserialize().unwrap())
                }
                Poll::Ready(_) => return None,
                Poll::Pending if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Poll::Pending => return None,
            }
        }
    }
    fn expect_badge(value: (String, Props), count: i64) {
        assert_eq!(value.0, LAUNCHER);
        assert_eq!(value.1.len(), 2);
        assert_eq!(i64::try_from(&value.1["count"]).unwrap(), count);
        assert_eq!(
            bool::try_from(&value.1["count-visible"]).unwrap(),
            count > 0
        );
    }

    #[test]
    fn launcher_badge_query_and_updates_follow_attention_even_while_the_menu_is_open() {
        let bus = crate::platform::linux_test_bus::Bus::new();
        let native = start(
            Rgb::new(255, 255, 255),
            Arc::new(|_, _| {}),
            Some(&bus.address),
        )
        .unwrap();
        let client = bus.connect();
        let owner = native.connection.unique_name().unwrap().to_string();
        let badge = Proxy::new(&client, owner.as_str(), BADGE_PATH, BADGE).unwrap();
        let mut updates = badge_updates(&client, &owner);
        expect_badge(badge.call("Query", &()).unwrap(), 0);
        offer_native(
            &native,
            Face::counts(4, 2, false),
            "initial".into(),
            entries("claude/p/a"),
            Some("2".into()),
        );
        expect_badge(next_badge(&mut updates, Duration::from_secs(3)).unwrap(), 2);
        expect_badge(badge.call("Query", &()).unwrap(), 2);

        lock(&native.tray).opened();
        offer_native(
            &native,
            Face::counts(9, 7, false),
            "new".into(),
            entries("codex/p/b"),
            Some("7".into()),
        );
        expect_badge(next_badge(&mut updates, Duration::from_secs(3)).unwrap(), 7);
        expect_badge(badge.call("Query", &()).unwrap(), 7);
        assert_eq!(lock(&native.tray).tooltip, "initial");
        lock(&native.tray).event(0, "closed");
        assert_eq!(lock(&native.tray).tooltip, "new");
        offer_native(
            &native,
            Face::counts(9, 7, false),
            "new".into(),
            entries("codex/p/b"),
            Some("7".into()),
        );
        assert!(
            next_badge(&mut updates, Duration::from_millis(80)).is_none(),
            "unchanged count does not publish again"
        );
        offer_native(
            &native,
            Face::counts(9, 0, false),
            "unavailable".into(),
            entries("codex/p/b"),
            None,
        );
        expect_badge(next_badge(&mut updates, Duration::from_secs(3)).unwrap(), 0);
        expect_badge(badge.call("Query", &()).unwrap(), 0);
        offer_native(
            &native,
            Face::counts(9, 3, false),
            "three".into(),
            entries("codex/p/b"),
            Some("3".into()),
        );
        expect_badge(next_badge(&mut updates, Duration::from_secs(3)).unwrap(), 3);
        drop(native);
        expect_badge(next_badge(&mut updates, Duration::from_secs(3)).unwrap(), 0);
    }

    #[test]
    fn shell_registration_coalesces_a_republish_without_a_new_attention_count() {
        let bus = crate::platform::linux_test_bus::Bus::new();
        let native = start(
            Rgb::new(255, 255, 255),
            Arc::new(|_, _| {}),
            Some(&bus.address),
        )
        .unwrap();
        let client = bus.connect();
        let mut updates = badge_updates(&client, native.connection.unique_name().unwrap().as_str());
        offer_native(
            &native,
            Face::counts(8, 4, false),
            "four".into(),
            entries("claude/p/a"),
            Some("4".into()),
        );
        expect_badge(next_badge(&mut updates, Duration::from_secs(3)).unwrap(), 4);
        let shell = bus.connect();
        shell.request_name(SHELLS[0]).unwrap();
        shell.request_name(SHELLS[1]).unwrap();
        expect_badge(next_badge(&mut updates, Duration::from_secs(3)).unwrap(), 4);
        assert!(
            next_badge(&mut updates, Duration::from_millis(100)).is_none(),
            "both registrations use one coalesced publication"
        );
        shell.release_name(SHELLS[1]).unwrap();
        shell.request_name(SHELLS[1]).unwrap();
        expect_badge(next_badge(&mut updates, Duration::from_secs(3)).unwrap(), 4);
        native.clear_badge();
        expect_badge(next_badge(&mut updates, Duration::from_secs(3)).unwrap(), 0);
        offer_native(
            &native,
            Face::counts(9, 8, false),
            "late".into(),
            entries("codex/p/b"),
            Some("8".into()),
        );
        let owner = native.connection.unique_name().unwrap().to_string();
        let badge = Proxy::new(&client, owner.as_str(), BADGE_PATH, BADGE).unwrap();
        expect_badge(badge.call("Query", &()).unwrap(), 0);
        assert!(
            next_badge(&mut updates, Duration::from_millis(100)).is_none(),
            "shutdown cannot revive the badge"
        );
        drop(native);
    }

    #[test]
    fn accounted_badge_closure_preserves_sni_menu_activation_and_refuses_late_republish() {
        use zerus_client_core::desktop::drain::{Gate, Observation, Role};
        let bus = crate::platform::linux_test_bus::Bus::new();
        let gate = Gate::new(1).unwrap();
        let (scope, control, startup) =
            badge::Scope::new(gate.admit(Role::Badges).unwrap()).unwrap();
        let (tx, rx) = mpsc::channel();
        let native = start_with_badge(
            Rgb::new(255, 255, 255),
            Arc::new(move |a, t| {
                tx.send((a, t)).unwrap();
            }),
            Some(&bus.address),
            Some(scope.clone()),
        )
        .unwrap();
        scope.ready();
        startup.complete();
        let client = bus.connect();
        let owner = native.connection.unique_name().unwrap().to_string();
        let mut updates = badge_updates(&client, &owner);
        let query = Proxy::new(&client, owner.as_str(), BADGE_PATH, BADGE).unwrap();
        offer_native(
            &native,
            Face::counts(8, 4, false),
            "four".into(),
            entries("claude/p/a"),
            Some("4".into()),
        );
        expect_badge(next_badge(&mut updates, Duration::from_secs(5)).unwrap(), 4);
        expect_badge(query.call("Query", &()).unwrap(), 4);
        control.close().unwrap();
        let closed = gate.close();
        assert_eq!(
            closed.wait_until(Instant::now() + Duration::from_secs(5)),
            Observation::Idle
        );
        expect_badge(next_badge(&mut updates, Duration::from_secs(5)).unwrap(), 0);
        assert!(!control.snapshot().unwrap().pinned);
        assert!(
            !native.stopped.load(Ordering::Acquire),
            "UI worker remains alive"
        );
        assert!(query.call::<_, _, (String, Props)>("Query", &()).is_err());
        offer_native(
            &native,
            Face::counts(9, 7, false),
            "new tooltip".into(),
            entries("codex/p/b"),
            Some("7".into()),
        );
        let item = Proxy::new(&client, owner.as_str(), PATH, SNI).unwrap();
        let tooltip: (String, Vec<Icon>, String, String) = item.get_property("ToolTip").unwrap();
        assert_eq!(tooltip.2, "new tooltip");
        item.call::<_, _, ()>("ProvideXdgActivationToken", &("after-badge-close",))
            .unwrap();
        item.call::<_, _, ()>("Activate", &(0_i32, 0_i32)).unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            (MenuAction::Open, Some("after-badge-close".into()))
        );
        let menu = Proxy::new(&client, owner.as_str(), MENU, DBUSMENU).unwrap();
        let (_, layout): (u32, Layout) = menu
            .call("GetLayout", &(0_i32, -1_i32, Vec::<String>::new()))
            .unwrap();
        assert!(!layout.2.is_empty());
        let id = lock(&native.tray).root.children[2].id;
        menu.call::<_, _, ()>("Event", &(id, "clicked", Value::from(0_i32), 0_u32))
            .unwrap();
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(5)).unwrap().0,MenuAction::Session { key,.. } if key == "run:codex/p/b")
        );
        let shell = bus.connect();
        shell.request_name(SHELLS[0]).unwrap();
        shell.request_name(SHELLS[1]).unwrap();
        assert!(
            next_badge(&mut updates, Duration::from_millis(1200)).is_none(),
            "closed badge cannot be republished by shell churn or UI offers"
        );
        assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
    }

    #[test]
    fn actual_startup_barrier_survives_close_after_interfaces_exist() {
        use zerus_client_core::desktop::drain::{Gate, Observation, Role};
        let bus = crate::platform::linux_test_bus::Bus::new();
        let gate = Gate::new(1).unwrap();
        let (scope, control, startup) =
            badge::Scope::new(gate.admit(Role::Badges).unwrap()).unwrap();
        let native = start_with_badge(
            Rgb::new(255, 255, 255),
            Arc::new(|_, _| {}),
            Some(&bus.address),
            Some(scope.clone()),
        )
        .unwrap();
        control.close().unwrap();
        let closed = gate.close();
        assert_eq!(
            closed.wait_until(Instant::now() + Duration::from_millis(30)),
            Observation::Pending
        );
        scope.ready();
        startup.complete();
        assert_eq!(
            closed.wait_until(Instant::now() + Duration::from_secs(5)),
            Observation::Idle
        );
        let client = bus.connect();
        let item = Proxy::new(
            &client,
            native.connection.unique_name().unwrap().as_str(),
            PATH,
            SNI,
        )
        .unwrap();
        assert_eq!(item.get_property::<String>("Status").unwrap(), "Active");
    }

    #[test]
    fn discarded_closed_startup_remains_unknown_instead_of_idle() {
        use zerus_client_core::desktop::drain::{Gate, Observation, Role};
        let bus = crate::platform::linux_test_bus::Bus::new();
        let gate = Gate::new(1).unwrap();
        let (scope, control, startup) =
            badge::Scope::new(gate.admit(Role::Badges).unwrap()).unwrap();
        control.close().unwrap();
        let closed = gate.close();
        let native = start_with_badge(
            Rgb::new(255, 255, 255),
            Arc::new(|_, _| {}),
            Some(&bus.address),
            Some(scope.clone()),
        );
        // Closed before any native setup is refused; startup remains explicitly unknown.
        assert!(native.is_err());
        drop(startup);
        assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
        assert!(control.snapshot().unwrap().pinned);
    }

    fn entries(name: &str) -> Vec<Entry> {
        vec![
            Entry::Open,
            Entry::Label(Text::verbatim("fixture")),
            Entry::Session {
                computer: "fixture".into(),
                key: format!("run:{name}"),
                name: name.into(),
                text: Text::verbatim(name),
                running: false,
                tty: String::new(),
            },
            Entry::OpenIn(OpenMode::Automatic),
            Entry::Quit,
        ]
    }
    fn update(tray: &mut Tray, name: &str, count: usize) {
        let changes = tray
            .bar
            .offer(
                Face::counts(count, 1, false),
                format!("{count} sessions"),
                entries(name),
                None,
            )
            .changes;
        tray.apply(changes);
    }

    #[test]
    fn an_open_menu_freezes_its_face_tooltip_labels_and_exact_actions() {
        let (tx, rx) = mpsc::channel();
        let mut tray = Tray::new(
            Rgb::new(255, 255, 255),
            Arc::new(move |a, t| {
                tx.send((a, t)).unwrap();
            }),
        );
        update(&mut tray, "claude/R&D_with_underscores/a", 1);
        let first = tray.root.children[2].id;
        tray.opened();
        update(&mut tray, "codex/other/b", 2);
        assert_eq!(tray.tooltip, "1 sessions");
        assert_eq!(tray.root.children[2].id, first);
        tray.event(first, "clicked");
        assert!(
            matches!(rx.recv().unwrap().0,MenuAction::Session { key, .. } if key == "run:claude/R&D_with_underscores/a")
        );
        assert!(tray.event(0, "closed"));
        assert_eq!(tray.tooltip, "2 sessions");
        assert_ne!(tray.root.children[2].id, first);
        tray.event(first, "clicked");
        assert!(
            matches!(rx.recv().unwrap().0,MenuAction::Session { key, .. } if key == "run:claude/R&D_with_underscores/a")
        );
    }

    #[test]
    fn linux_modes_are_exclusive_and_dbusmenu_mnemonics_keep_literal_names() {
        let mut tray = Tray::new(Rgb::new(255, 255, 255), Arc::new(|_, _| {}));
        update(&mut tray, "claude/R&&D_name/a", 1);
        let shown: &str = (&tray.root.children[2].props["label"]).try_into().unwrap();
        assert_eq!(shown, "claude/R&D__name/a");
        let modes = &tray.root.children[3].children;
        assert_eq!(modes.len(), 4);
        assert_eq!(
            modes
                .iter()
                .map(|node| i32::try_from(&node.props["toggle-state"]).unwrap())
                .collect::<Vec<_>>(),
            [1, 0, 0, 0]
        );
    }

    #[test]
    fn qt_tile_sizes_have_argb_pixels_and_attention_changes_the_icon() {
        for size in [16, 22, 24, 32, 48] {
            let normal = render(&Face::counts(5, 0, false), size, Rgb::new(255, 255, 255)).unwrap();
            let attention =
                render(&Face::counts(5, 1, true), size, Rgb::new(255, 255, 255)).unwrap();
            assert_eq!(normal.2.len(), (size * size * 4) as usize);
            assert_ne!(normal.2, attention.2);
            assert!(attention
                .2
                .as_chunks::<4>()
                .0
                .contains(&[255, 0xf3, 0xbd, 0x64]));
        }
    }

    struct Watcher(mpsc::Sender<String>);
    #[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
    impl Watcher {
        fn register_status_notifier_item(&self, service: String) {
            self.0.send(service).unwrap();
        }
    }

    #[test]
    fn private_bus_exports_primary_activation_tooltip_icons_and_a_tracking_menu() {
        let bus = crate::platform::linux_test_bus::Bus::new();
        let (registered, registration) = mpsc::channel();
        let _watcher = zbus::blocking::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name(WATCHER)
            .unwrap()
            .serve_at("/StatusNotifierWatcher", Watcher(registered.clone()))
            .unwrap()
            .build()
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let native = start(
            Rgb::new(255, 255, 255),
            Arc::new(move |a, t| {
                tx.send((a, t)).unwrap();
            }),
            Some(&bus.address),
        )
        .unwrap();
        let name = registration.recv_timeout(Duration::from_secs(5)).unwrap();
        offer_native(
            &native,
            Face::counts(1, 1, false),
            "1 needs attention\n1 here".into(),
            entries("claude/p/a"),
            None,
        );
        let connection = bus.connect();
        let item = Proxy::new(&connection, name.as_str(), PATH, SNI).unwrap();
        assert!(!item.get_property::<bool>("ItemIsMenu").unwrap());
        assert_eq!(item.get_property::<String>("Status").unwrap(), "Active");
        assert_eq!(
            item.get_property::<Vec<Icon>>("IconPixmap").unwrap().len(),
            5
        );
        let tooltip: (String, Vec<Icon>, String, String) = item.get_property("ToolTip").unwrap();
        assert_eq!(tooltip.2, "1 needs attention\n1 here");
        item.call::<_, _, ()>("ProvideXdgActivationToken", &("fixture-activation",))
            .unwrap();
        item.call::<_, _, ()>("Activate", &(10_i32, 20_i32))
            .unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            (MenuAction::Open, Some("fixture-activation".into()))
        );
        let menu = Proxy::new(&connection, name.as_str(), MENU, DBUSMENU).unwrap();
        let (revision, layout): (u32, Layout) = menu
            .call("GetLayout", &(0_i32, -1_i32, Vec::<String>::new()))
            .unwrap();
        assert!(!layout.2.is_empty());
        let first = lock(&native.tray).root.children[2].id;
        assert!(!menu.call::<_, _, bool>("AboutToShow", &(0_i32,)).unwrap());
        offer_native(
            &native,
            Face::counts(2, 0, false),
            "changed".into(),
            entries("codex/p/b"),
            None,
        );
        let (held, _): (u32, Layout) = menu
            .call("GetLayout", &(0_i32, -1_i32, Vec::<String>::new()))
            .unwrap();
        assert_eq!(held, revision);
        menu.call::<_, _, ()>("Event", &(first, "clicked", Value::from(0_i32), 0_u32))
            .unwrap();
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(5)).unwrap().0,MenuAction::Session { key, .. } if key == "run:claude/p/a")
        );
        menu.call::<_, _, ()>("Event", &(0_i32, "closed", Value::from(0_i32), 0_u32))
            .unwrap();
        let (settled, _): (u32, Layout) = menu
            .call("GetLayout", &(0_i32, -1_i32, Vec::<String>::new()))
            .unwrap();
        assert!(settled > revision);
        let all: Vec<(i32, Props)> = menu
            .call(
                "GetGroupProperties",
                &(Vec::<i32>::new(), Vec::<String>::new()),
            )
            .unwrap();
        assert!(all.len() > 5);
        _watcher.release_name(WATCHER).unwrap();
        let _replacement = zbus::blocking::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name(WATCHER)
            .unwrap()
            .serve_at("/StatusNotifierWatcher", Watcher(registered))
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            registration.recv_timeout(Duration::from_secs(10)).unwrap(),
            name,
            "Plasma's replacement watcher gets the existing item, without another client"
        );
    }
}
