//! One confirmed window level for Settings and the Rail. Settings and native
//! changes share the same store. Qt's Wayland backend requires a mapped window;
//! other platforms apply the saved level before the first show.
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tokio::sync::oneshot;
use zerus_client_core::settings::Settings;

use crate::settings::{Store, ALWAYS_ON_TOP};

pub const NORMAL: &str =
    "Files and terminals opened from Zerus may appear behind it while this is on.";
pub const UNAVAILABLE: &str = "Keep Above is unavailable in this Zerus session. On Wayland, KDE Plasma 6 with KWin scripting is required. You can use your desktop's window menu or rules.";
pub const CHECKING: &str =
    "Checking whether this desktop supports keeping Zerus above other windows…";
#[cfg(target_os = "linux")]
const DENIED: &str =
    "KWin did not apply this preference. Check the window's Keep Above rule in KDE settings.";
#[cfg(target_os = "linux")]
const AMBIGUOUS: &str = "Zerus could not uniquely identify its window in KWin. Close duplicate workspace windows and reopen Zerus from the tray to try again.";

#[derive(Clone, Debug, Serialize)]
pub struct View {
    pub supported: bool,
    pub on: bool,
    pub busy: bool,
    pub hint: String,
    pub revision: u64,
}
struct Shared {
    view: View,
    epoch: u64,
    #[cfg(any(windows, test))]
    opening: bool,
    #[cfg(any(windows, test))]
    open_epoch: Option<u64>,
}
pub struct Layer {
    shared: Arc<Mutex<Shared>>,
    sender: mpsc::SyncSender<Work>,
}
enum Work {
    #[cfg(windows)]
    Open,
    Show(Box<WebviewWindow>, u64),
    Set(bool, u64, oneshot::Sender<Result<(Value, u64), String>>),
}

pub fn init(app: &AppHandle, display: Option<&str>) {
    let wayland =
        cfg!(target_os = "linux") && display.is_some_and(|name| name.starts_with("wayland"));
    let shared = Arc::new(Mutex::new(Shared {
        epoch: 0,
        #[cfg(any(windows, test))]
        opening: false,
        #[cfg(any(windows, test))]
        open_epoch: None,
        view: View {
            supported: !wayland,
            on: false,
            busy: false,
            hint: if wayland { UNAVAILABLE } else { NORMAL }.into(),
            revision: 0,
        },
    }));
    let (sender, receiver) = mpsc::sync_channel(8);
    let worker = Worker {
        app: app.clone(),
        settings: app.state::<Store>().0.clone(),
        shared: shared.clone(),
        wayland,
        window: None,
        id: String::new(),
        #[cfg(target_os = "linux")]
        kwin: None,
    };
    let started = std::thread::Builder::new()
        .name("window-layer".into())
        .spawn(move || worker.run(receiver));
    if let Err(error) = started {
        eprintln!("zerus-client: window layer: {error}");
    }
    app.manage(Layer { shared, sender });
}

#[cfg(any(windows, test))]
fn enqueue_open(
    shared: &Mutex<Shared>,
    send: impl FnOnce() -> Result<(), ()>,
    prepare: impl FnOnce(),
) -> Result<(), ()> {
    let mut state = shared.lock().unwrap();
    if state.opening && state.open_epoch == Some(state.epoch) {
        return Ok(());
    }
    let epoch = state.epoch + 1;
    if !state.opening {
        send()?;
        prepare();
    }
    state.opening = true;
    state.open_epoch = Some(epoch);
    state.epoch = epoch;
    state.view.busy = true;
    state.view.revision += 1;
    Ok(())
}

#[cfg(any(windows, test))]
fn finish_open(shared: &Mutex<Shared>) -> Option<u64> {
    let mut state = shared.lock().unwrap();
    state.opening = false;
    state.open_epoch.take()
}

#[cfg(windows)]
pub fn open(app: &AppHandle) -> Result<(), String> {
    let layer = app
        .try_state::<Layer>()
        .ok_or("Window presentation is not ready")?;
    // Native callbacks call this on the main thread. Only the first admitted
    // creation reserves a new inbox page; duplicates must not invalidate a
    // document which is already loading on the worker.
    let creating = layer.shared.lock().unwrap().opening;
    let new_page = !creating && crate::main_window(app).is_none();
    enqueue_open(
        &layer.shared,
        || layer.sender.try_send(Work::Open).map_err(|_| ()),
        || {
            if new_page {
                crate::desk::page_loading(app);
            }
        },
    )
    .map_err(|()| "Window presentation is unavailable".into())
}

/// Called on the UI thread. It never waits for settings, the compositor or disk.
pub fn show(app: &AppHandle, window: WebviewWindow) -> bool {
    let Some(layer) = app.try_state::<Layer>() else {
        return false;
    };
    let mut state = layer.shared.lock().unwrap();
    let epoch = state.epoch + 1;
    if layer
        .sender
        .try_send(Work::Show(Box::new(window), epoch))
        .is_err()
    {
        return false;
    }
    state.epoch = epoch;
    #[cfg(any(windows, test))]
    {
        state.open_epoch = None;
    }
    state.view.busy = true;
    if !state.view.supported {
        state.view.hint = CHECKING.into();
    }
    state.view.revision += 1;
    let _ = app.emit("window-layer-changed", &state.view);
    true
}
pub fn hidden(app: &AppHandle) {
    if let Some(layer) = app.try_state::<Layer>() {
        let mut state = layer.shared.lock().unwrap();
        state.epoch += 1;
        #[cfg(any(windows, test))]
        {
            state.open_epoch = None;
        }
        state.view.busy = false;
    }
}
#[tauri::command]
pub fn window_layer_state(layer: State<'_, Layer>) -> View {
    layer.shared.lock().unwrap().view.clone()
}
pub async fn request(app: &AppHandle, on: bool) -> Result<(Value, u64), String> {
    let layer = app.state::<Layer>();
    let (send, receive) = oneshot::channel();
    {
        let mut state = layer.shared.lock().unwrap();
        if !state.view.supported {
            return Err(state.view.hint.clone());
        }
        if state.view.busy {
            return Err("A window layer change is already pending.".into());
        }
        layer
            .sender
            .try_send(Work::Set(on, state.epoch, send))
            .map_err(|_| UNAVAILABLE.to_owned())?;
        state.view.busy = true;
        state.view.revision += 1;
        let _ = app.emit("window-layer-changed", &state.view);
    }
    receive.await.map_err(|_| UNAVAILABLE.to_owned())?
}

struct Worker {
    app: AppHandle,
    settings: Settings,
    shared: Arc<Mutex<Shared>>,
    wayland: bool,
    window: Option<WebviewWindow>,
    id: String,
    #[cfg(target_os = "linux")]
    kwin: Option<crate::platform::linux_window_layer::KWin>,
}
impl Worker {
    fn current(&self, epoch: u64) -> bool {
        self.shared.lock().unwrap().epoch == epoch
    }
    fn publish(
        &self,
        epoch: u64,
        supported: bool,
        on: bool,
        hint: &str,
        persist: bool,
    ) -> Result<(Value, u64), String> {
        let mut state = self.shared.lock().unwrap();
        if state.epoch != epoch {
            return Err(UNAVAILABLE.into());
        }
        state.view = View {
            supported,
            on,
            busy: false,
            hint: hint.into(),
            revision: state.view.revision + 1,
        };
        // The settings listener only emits; it never calls back into this lock.
        let result = if persist {
            self.settings.set_tracked(ALWAYS_ON_TOP, json!(on))
        } else {
            let (values, revision) = self.settings.all_tracked();
            Ok((
                values.get(ALWAYS_ON_TOP).cloned().unwrap_or(json!(false)),
                revision,
            ))
        };
        let _ = self.app.emit("window-layer-changed", &state.view);
        result
    }
    fn present(&self, window: WebviewWindow, epoch: u64) {
        let shared = self.shared.clone();
        let app = self.app.clone();
        let _ = self.app.run_on_main_thread(move || {
            if shared.lock().unwrap().epoch != epoch {
                return;
            }
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
            crate::lifecycle::sync_policy(&app);
        });
    }
    fn run(mut self, receive: mpsc::Receiver<Work>) {
        // This load may include the one-time Qt seed. No UI-thread I/O.
        let _ = self.settings.flag(ALWAYS_ON_TOP);
        loop {
            let work = match receive.recv_timeout(Duration::from_millis(1500)) {
                Ok(work) => work,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.poll();
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            match work {
                #[cfg(windows)]
                Work::Open => {
                    // This is the existing background window-layer thread,
                    // never a main-thread/event-handler WebView2 constructor.
                    let window = crate::main_window(&self.app)
                        .map(Ok)
                        .unwrap_or_else(|| crate::create_main_window(&self.app));
                    let requested = finish_open(&self.shared);
                    match (window, requested) {
                        (Ok(window), Some(epoch)) => self.show(window, epoch),
                        (Err(error), requested) => {
                            if requested.is_some_and(|epoch| self.current(epoch)) {
                                let mut state = self.shared.lock().unwrap();
                                state.view.busy = false;
                                state.view.revision += 1;
                            }
                            eprintln!("zerus-client: could not create the Windows window: {error}");
                        }
                        (Ok(_), None) => {}
                    }
                }
                Work::Show(window, epoch) => {
                    self.show(*window, epoch);
                }
                Work::Set(on, epoch, reply) => {
                    if !self.current(epoch) {
                        let _ = reply.send(Err(UNAVAILABLE.into()));
                        continue;
                    }
                    let result = self.change(epoch, on);
                    let _ = reply.send(result);
                }
            }
        }
    }
    fn show(&mut self, window: WebviewWindow, epoch: u64) {
        if !self.current(epoch) {
            return;
        }
        let first = self.window.is_none();
        let desired = self.settings.flag(ALWAYS_ON_TOP);
        self.window = Some(window.clone());
        if self.wayland {
            self.id.clear();
            self.present(window, epoch);
            self.probe(epoch, desired);
        } else {
            let result = if first {
                window.set_always_on_top(desired)
            } else {
                Ok(())
            };
            let on = if first {
                desired
            } else {
                window.is_always_on_top().unwrap_or(desired)
            };
            let _ = self.publish(
                epoch,
                result.is_ok(),
                on,
                if result.is_ok() { NORMAL } else { UNAVAILABLE },
                result.is_ok(),
            );
            self.present(window, epoch);
        }
    }
    fn change(&mut self, epoch: u64, on: bool) -> Result<(Value, u64), String> {
        if self.wayland {
            #[cfg(target_os = "linux")]
            if let (Some(kwin), Some(window)) = (&self.kwin, &self.window) {
                let result = kwin.request(&window.title().unwrap_or_default(), &self.id, Some(on));
                return self.confirm(epoch, result, true);
            }
            return Err(UNAVAILABLE.into());
        }
        let result = self
            .window
            .as_ref()
            .ok_or_else(|| UNAVAILABLE.to_owned())?
            .set_always_on_top(on);
        let ok = result.is_ok();
        let saved = self.publish(
            epoch,
            ok,
            if ok { on } else { !on },
            if ok { NORMAL } else { UNAVAILABLE },
            ok,
        );
        if ok {
            saved
        } else {
            Err(UNAVAILABLE.into())
        }
    }
    fn poll(&mut self) {
        let Some(window) = &self.window else { return };
        if !window.is_visible().unwrap_or(false) {
            return;
        }
        let epoch = self.shared.lock().unwrap().epoch;
        if self.wayland {
            #[cfg(target_os = "linux")]
            if let Some(kwin) = &self.kwin {
                if !self.id.is_empty() {
                    let result = kwin.inspect(&self.id);
                    let _ = self.confirm(epoch, result, false);
                }
            }
        } else if let Ok(on) = window.is_always_on_top() {
            let _ = self.publish(epoch, true, on, NORMAL, true);
        }
    }
    fn probe(&mut self, epoch: u64, desired: bool) {
        #[cfg(not(target_os = "linux"))]
        let _ = desired;
        #[cfg(target_os = "linux")]
        {
            use crate::platform::linux_window_layer::KWin;
            if self.kwin.is_none() {
                let app_id = self.app.config().identifier.clone();
                self.kwin = KWin::session(app_id).ok();
            }
            let mut result = None;
            for _ in 0..8 {
                std::thread::sleep(Duration::from_millis(200));
                if !self.current(epoch) {
                    return;
                }
                let Some(kwin) = &self.kwin else { break };
                let caption = self
                    .window
                    .as_ref()
                    .and_then(|window| window.title().ok())
                    .unwrap_or_default();
                let reply = kwin.request(&caption, "", None);
                let missing = reply.error == "missing";
                result = Some(reply);
                if !missing {
                    break;
                }
            }
            if let Some(mut result) = result {
                if !result.id.is_empty() && result.on != desired {
                    result = self
                        .kwin
                        .as_ref()
                        .unwrap()
                        .request("", &result.id, Some(desired));
                }
                let _ = self.confirm(epoch, result, false);
                return;
            }
        }
        let _ = self.publish(epoch, false, false, UNAVAILABLE, false);
    }
    #[cfg(target_os = "linux")]
    fn confirm(
        &mut self,
        epoch: u64,
        result: crate::platform::linux_window_layer::Reply,
        requested: bool,
    ) -> Result<(Value, u64), String> {
        if !self.current(epoch) {
            return Err(UNAVAILABLE.into());
        }
        let supported = !result.id.is_empty();
        self.id = result.id;
        let hint = match result.error.as_str() {
            "denied" => DENIED,
            "ambiguous" => AMBIGUOUS,
            "" if supported => NORMAL,
            _ => UNAVAILABLE,
        };
        let previous = self.shared.lock().unwrap().view.clone();
        let hint = if !requested
            && result.error.is_empty()
            && previous.on == result.on
            && previous.hint == DENIED
        {
            DENIED
        } else {
            hint
        };
        let on = if supported { result.on } else { previous.on };
        let saved = self.publish(epoch, supported, on, hint, supported);
        if requested && !result.error.is_empty() {
            Err(hint.into())
        } else {
            saved
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn state() -> Mutex<Shared> {
        Mutex::new(Shared {
            epoch: 0,
            opening: false,
            open_epoch: None,
            view: View {
                supported: true,
                on: false,
                busy: false,
                hint: NORMAL.into(),
                revision: 0,
            },
        })
    }

    #[test]
    fn repeated_activations_keep_one_inflight_creation_and_the_original_epoch() {
        let shared = state();
        let sends = Cell::new(0);
        let preparations = Cell::new(0);
        for _ in 0..32 {
            enqueue_open(
                &shared,
                || {
                    sends.set(sends.get() + 1);
                    Ok(())
                },
                || preparations.set(preparations.get() + 1),
            )
            .unwrap();
        }
        assert_eq!(sends.get(), 1);
        assert_eq!(preparations.get(), 1);
        assert_eq!(shared.lock().unwrap().epoch, 1);
        assert_eq!(finish_open(&shared), Some(1));
        assert!(!shared.lock().unwrap().opening);
    }

    #[test]
    fn a_failed_queue_does_not_reserve_creation_or_renew_the_epoch() {
        let shared = state();
        assert!(enqueue_open(
            &shared,
            || Err(()),
            || panic!("refused queue must not change page readiness")
        )
        .is_err());
        let state = shared.lock().unwrap();
        assert!(!state.opening);
        assert_eq!(state.open_epoch, None);
        assert_eq!(state.epoch, 0);
        assert!(!state.view.busy);
    }

    #[test]
    fn hide_cancels_the_old_show_but_a_new_activation_survives_inflight_creation() {
        let shared = state();
        enqueue_open(&shared, || Ok(()), || {}).unwrap();
        {
            let mut state = shared.lock().unwrap();
            state.epoch += 1;
            state.open_epoch = None;
        }
        // Hide alone never makes the completed creation show its window.
        assert_eq!(finish_open(&shared), None);
        enqueue_open(&shared, || Ok(()), || {}).unwrap();
        {
            let mut state = shared.lock().unwrap();
            state.epoch += 1;
            state.open_epoch = None;
        }
        enqueue_open(
            &shared,
            || panic!("reuse the one in-flight creation"),
            || panic!("keep the in-flight document page"),
        )
        .unwrap();
        assert_eq!(finish_open(&shared), Some(5));
    }
}
