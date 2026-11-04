//! The page's last debounced edit, before the one existing exit hook runs.
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State, Webview};

pub const EXIT_FLUSH: Duration = Duration::from_millis(150);

#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    Waiting {
        nonce: String,
        code: i32,
        deadline: Instant,
        acknowledged: bool,
    },
    Authorized(i32),
    Consumed,
}
#[derive(Default)]
struct Shared {
    phase: Mutex<Phase>,
    wake: Condvar,
}
#[derive(Clone, Default)]
pub struct ExitFlush(Arc<Shared>);
#[derive(Debug, PartialEq)]
enum Begin {
    Waiting(String),
    Immediate,
    Coalesced,
}
#[derive(Debug, PartialEq)]
pub(crate) enum Decision {
    Prevent,
    Flush(i32),
    Allow,
}

impl ExitFlush {
    fn lock(&self) -> MutexGuard<'_, Phase> {
        self.0.phase.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn begin(&self, code: i32, page: bool) -> Begin {
        let nonce = uuid::Uuid::new_v4().to_string();
        let mut phase = self.lock();
        if !matches!(*phase, Phase::Idle) {
            return Begin::Coalesced;
        }
        if !page {
            *phase = Phase::Authorized(code);
            return Begin::Immediate;
        }
        *phase = Phase::Waiting {
            nonce: nonce.clone(),
            code,
            deadline: Instant::now() + EXIT_FLUSH,
            acknowledged: false,
        };
        Begin::Waiting(nonce)
    }

    pub(crate) fn acknowledge(&self, nonce: &str, window: &str) -> bool {
        if window != crate::MAIN_WINDOW {
            return false;
        }
        let mut phase = self.lock();
        let Phase::Waiting {
            nonce: expected,
            acknowledged,
            ..
        } = &mut *phase
        else {
            return false;
        };
        if expected != nonce || *acknowledged {
            return false;
        }
        *acknowledged = true;
        self.0.wake.notify_all();
        true
    }

    fn wait(&self, nonce: &str) -> bool {
        let mut phase = self.lock();
        loop {
            let Phase::Waiting {
                nonce: expected,
                code,
                deadline,
                acknowledged,
            } = &*phase
            else {
                return false;
            };
            if expected != nonce {
                return false;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if *acknowledged || left.is_zero() {
                let code = *code;
                *phase = Phase::Authorized(code);
                return true;
            }
            phase = self
                .0
                .wake
                .wait_timeout(phase, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn release(&self, nonce: &str) -> bool {
        let mut phase = self.lock();
        let Phase::Waiting {
            nonce: expected,
            code,
            ..
        } = &*phase
        else {
            return false;
        };
        if expected != nonce {
            return false;
        }
        let code = *code;
        *phase = Phase::Authorized(code);
        true
    }

    pub(crate) fn consume(&self, code: Option<i32>) -> bool {
        let mut phase = self.lock();
        if !matches!(*phase,Phase::Authorized(expected) if code==Some(expected)) {
            return false;
        }
        *phase = Phase::Consumed;
        true
    }

    /// Shared by every native quit source; tests use a fake page and exit callback.
    pub(crate) fn quit(
        &self,
        code: i32,
        page: bool,
        emit: impl FnOnce(&str),
        finish: impl Fn(i32) + Clone + Send + 'static,
    ) {
        match self.begin(code, page) {
            Begin::Coalesced => {}
            Begin::Immediate => finish(code),
            Begin::Waiting(nonce) => {
                emit(&nonce);
                let worker = self.clone();
                let end = finish.clone();
                let ticket = nonce.clone();
                if std::thread::Builder::new()
                    .name("zerus-exit-flush".into())
                    .spawn(move || {
                        if worker.wait(&ticket) {
                            end(code);
                        }
                    })
                    .is_err()
                    && self.release(&nonce)
                {
                    finish(code);
                }
            }
        }
    }

    pub(crate) fn decision(&self, menu_bar: bool, code: Option<i32>) -> Decision {
        if menu_bar && code.is_none() {
            Decision::Prevent
        } else if self.consume(code) || code == Some(tauri::RESTART_EXIT_CODE) {
            Decision::Allow
        } else {
            Decision::Flush(code.unwrap_or(0))
        }
    }
}

/// All quits the client originates come here. Waiting never blocks the run loop.
pub(crate) fn quit_with_flush(app: &AppHandle, code: i32) {
    #[cfg(feature = "live")]
    if let Some(delays) = app.try_state::<crate::action_delay::Delays>() {
        delays.cancel_all(true);
    }
    let end = app.clone();
    app.state::<ExitFlush>().quit(
        code,
        crate::main_window(app).is_some(),
        |nonce| {
            let _ = app.emit_to(crate::MAIN_WINDOW, "exit-flush", nonce);
        },
        move |code| finish_exit(end.clone(), code),
    );
}

fn finish_exit(app: AppHandle, code: i32) {
    #[cfg(feature = "live")]
    if let Some(service) = app
        .try_state::<crate::commands::AppState>()
        .and_then(|source| source.desktop_service())
    {
        // The draft handshake has ended. Join this GUI's owned proxy and view
        // drain off the event loop before requesting the final native exit.
        tauri::async_runtime::spawn(finish_service_exit(service, move || app.exit(code)));
        return;
    }
    app.exit(code);
}

#[cfg(feature = "live")]
pub(crate) async fn finish_service_exit(
    service: Arc<crate::desktop_service::Service>,
    finish: impl FnOnce(),
) {
    finish_service_exit_with_deadline(service, finish, std::time::Duration::from_secs(5)).await;
}

#[cfg(feature = "live")]
pub(crate) async fn finish_service_exit_with_deadline(
    service: Arc<crate::desktop_service::Service>,
    finish: impl FnOnce(),
    wait: std::time::Duration,
) {
    match tokio::time::timeout(wait, service.join()).await {
        Ok(Ok(())) => (),
        Ok(Err(error)) => eprintln!("zerus-client: service shutdown: {error}"),
        Err(_) => eprintln!("zerus-client: service shutdown exceeded the quit deadline"),
    }
    finish();
}

#[tauri::command]
pub fn exit_flush_ack(window: Webview, flush: State<'_, ExitFlush>, nonce: String) {
    flush.acknowledge(&nonce, window.label());
}

#[cfg(target_os = "macos")]
pub(crate) const QUIT_ID: &str = "zerus.quit-with-flush";

/// Tauri's own menu and item instances, with Cocoa's Show All and terminate: replaced.
#[cfg(target_os = "macos")]
pub(crate) fn app_menu(app: &AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{Menu, MenuItem, MenuItemKind, PredefinedMenuItem};
    let menu = Menu::default(app)?;
    let before = menu_snapshot(&menu)?;
    let items = menu.items()?;
    let application = items
        .first()
        .and_then(MenuItemKind::as_submenu)
        .ok_or_else(|| std::io::Error::other("Default application menu is missing"))?;
    let entries = application.items()?;
    let position = entries
        .len()
        .checked_sub(1)
        .ok_or_else(|| std::io::Error::other("Default application menu is empty"))?;
    let show_all_position = position
        .checked_sub(1)
        .ok_or_else(|| std::io::Error::other("Default Quit separator is missing"))?;
    let MenuItemKind::Predefined(quit) = &entries[position] else {
        return Err(std::io::Error::other("Default Quit item is missing").into());
    };
    let caption = quit.text()?;
    if caption != "Quit" && !caption.starts_with("Quit ") {
        return Err(std::io::Error::other("Default Quit caption changed").into());
    }
    let replacement = MenuItem::with_id(app, QUIT_ID, &caption, true, Some("CmdOrCtrl+Q"))?;
    let show_all = PredefinedMenuItem::show_all(app, None)?;
    let show_all_id = show_all.id().clone();
    application.remove(quit)?;
    application.insert(&show_all, show_all_position)?;
    application.insert(&replacement, position + 1)?;
    let expected =
        expected_menu_with_show_all_and_quit(&before, position, show_all_id.as_ref(), QUIT_ID)
            .ok_or_else(|| std::io::Error::other("Default application menu changed"))?;
    if expected != menu_snapshot(&menu)? {
        return Err(std::io::Error::other("Default menu changed beyond Show All and Quit").into());
    }
    Ok(menu)
}

/// No accelerator getter exists in Tauri/muda. Unchanged ids are the same
/// native item instances; their accelerators and predefined editing actions stay intact.
#[cfg(target_os = "macos")]
fn menu_snapshot(menu: &tauri::menu::Menu<tauri::Wry>) -> tauri::Result<serde_json::Value> {
    use tauri::menu::MenuItemKind;
    fn entries(items: Vec<MenuItemKind<tauri::Wry>>) -> tauri::Result<serde_json::Value> {
        let mut out = Vec::new();
        for item in items {
            let (kind, caption, children) = match &item {
                MenuItemKind::MenuItem(i) => ("item", i.text()?, serde_json::Value::Null),
                MenuItemKind::Predefined(i) => ("predefined", i.text()?, serde_json::Value::Null),
                MenuItemKind::Check(i) => ("check", i.text()?, serde_json::Value::Null),
                MenuItemKind::Icon(i) => ("icon", i.text()?, serde_json::Value::Null),
                MenuItemKind::Submenu(i) => ("submenu", i.text()?, entries(i.items()?)?),
            };
            out.push(serde_json::json!({"id":item.id().as_ref(),"kind":kind,"caption":caption,"children":children}));
        }
        Ok(out.into())
    }
    entries(menu.items()?)
}

#[cfg(any(target_os = "macos", test))]
fn expected_menu_with_show_all_and_quit(
    before: &serde_json::Value,
    quit_position: usize,
    show_all_id: &str,
    quit_id: &str,
) -> Option<serde_json::Value> {
    let show_all_position = quit_position.checked_sub(1)?;
    let mut expected = before.clone();
    let entries = expected.get_mut(0)?.get_mut("children")?.as_array_mut()?;
    let mut quit = entries.get(quit_position)?.clone();
    quit["id"] = quit_id.into();
    quit["kind"] = "item".into();
    entries.remove(quit_position);
    entries.insert(
        show_all_position,
        serde_json::json!({"id":show_all_id,"kind":"predefined","caption":"Show All","children":null}),
    );
    entries.insert(quit_position + 1, quit);
    Some(expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    fn waiting(flush: &ExitFlush, code: i32) -> String {
        let Begin::Waiting(nonce) = flush.begin(code, true) else {
            panic!("page must be asked")
        };
        nonce
    }
    #[test]
    fn a_stalled_page_times_out_at_150_ms_without_hanging_the_exit_thread() {
        assert_eq!(EXIT_FLUSH.as_millis(), 150);
        let flush = ExitFlush::default();
        let start = Instant::now();
        let nonce = waiting(&flush, 17);
        let (done, ended) = std::sync::mpsc::channel();
        let worker = flush.clone();
        std::thread::spawn(move || {
            done.send(worker.wait(&nonce)).unwrap();
        });
        assert!(ended
            .recv_timeout(Duration::from_secs(2))
            .expect("exit exceeded its deadline"));
        assert!(start.elapsed() >= EXIT_FLUSH && start.elapsed() < Duration::from_secs(2));
        assert!(flush.consume(Some(17)));
    }
    #[test]
    fn an_acknowledgement_releases_quit_independently_of_the_timeout() {
        let flush = ExitFlush::default();
        let nonce = waiting(&flush, 6);
        // Separate the acknowledgement from the timer without relying on a
        // sub-150ms scheduler race on the shared build machines.
        if let Phase::Waiting { deadline, .. } = &mut *flush.lock() {
            *deadline = Instant::now() + Duration::from_secs(60);
        }
        assert!(flush.acknowledge(&nonce, crate::MAIN_WINDOW));
        let worker = flush.clone();
        let (done, ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            done.send(worker.wait(&nonce)).unwrap();
        });
        assert!(ended
            .recv_timeout(Duration::from_secs(2))
            .expect("acknowledgement was ignored"));
        assert!(flush.consume(Some(6)));
    }

    #[test]
    fn only_the_exact_nonce_and_main_page_acknowledge_once() {
        let flush = ExitFlush::default();
        let nonce = waiting(&flush, 0);
        assert!(!flush.acknowledge("stale", crate::MAIN_WINDOW));
        assert!(!flush.acknowledge(&nonce, "other"));
        assert!(flush.acknowledge(&nonce, crate::MAIN_WINDOW));
        assert!(!flush.acknowledge(&nonce, crate::MAIN_WINDOW));
        assert!(flush.wait(&nonce));
        assert!(!flush.acknowledge(&nonce, crate::MAIN_WINDOW));
        assert!(flush.consume(Some(0)));
    }
    #[test]
    fn repeated_quits_coalesce_and_keep_the_first_exit_code() {
        let flush = ExitFlush::default();
        let nonce = waiting(&flush, 23);
        assert_eq!(flush.begin(23, true), Begin::Coalesced);
        assert_eq!(flush.begin(99, true), Begin::Coalesced);
        flush.acknowledge(&nonce, crate::MAIN_WINDOW);
        flush.wait(&nonce);
        assert!(!flush.consume(Some(99)));
        assert!(flush.consume(Some(23)));
    }
    #[test]
    fn no_page_never_waits_and_authorization_is_consumed_by_one_exact_request() {
        let flush = ExitFlush::default();
        assert_eq!(flush.begin(7, false), Begin::Immediate);
        assert!(!flush.consume(None));
        assert!(!flush.consume(Some(8)));
        assert!(flush.consume(Some(7)));
        assert!(!flush.consume(Some(7)));
        assert_eq!(flush.decision(false, Some(7)), Decision::Flush(7));
        assert_eq!(flush.begin(7, false), Begin::Coalesced);
    }
    #[test]
    fn a_menu_bar_window_exit_stays_prevented_even_when_a_quit_is_authorized() {
        let flush = ExitFlush::default();
        flush.begin(0, false);
        assert_eq!(flush.decision(true, None), Decision::Prevent);
        assert_eq!(flush.decision(true, Some(0)), Decision::Allow);
        assert_eq!(flush.decision(true, Some(0)), Decision::Flush(0));
        assert_eq!(
            flush.decision(false, Some(tauri::RESTART_EXIT_CODE)),
            Decision::Allow
        );
    }
    #[test]
    fn default_menu_structure_adds_only_show_all_and_replaces_quit() {
        use serde_json::json;
        let before = json!([
            {"id":"application","kind":"submenu","caption":"Zerus","children":[
                {"id":"about","kind":"predefined","caption":"About Zerus","children":null},
                {"id":"separator-1","kind":"predefined","caption":"","children":null},
                {"id":"services","kind":"predefined","caption":"Services","children":null},
                {"id":"separator-2","kind":"predefined","caption":"","children":null},
                {"id":"hide","kind":"predefined","caption":"Hide Zerus","children":null},
                {"id":"hide-others","kind":"predefined","caption":"Hide Others","children":null},
                {"id":"separator-3","kind":"predefined","caption":"","children":null},
                {"id":"default-quit","kind":"predefined","caption":"Quit Zerus","children":null}]},
            {"id":"edit","kind":"submenu","caption":"Edit","children":[
                {"id":"undo","kind":"predefined","caption":"Undo","children":null},
                {"id":"redo","kind":"predefined","caption":"Redo","children":null},
                {"id":"cut","kind":"predefined","caption":"Cut","children":null},
                {"id":"copy","kind":"predefined","caption":"Copy","children":null},
                {"id":"paste","kind":"predefined","caption":"Paste","children":null},
                {"id":"select-all","kind":"predefined","caption":"Select All","children":null}]},
            {"id":"window","kind":"submenu","caption":"Window","children":[]}
        ]);
        let after = json!([
            {"id":"application","kind":"submenu","caption":"Zerus","children":[
                {"id":"about","kind":"predefined","caption":"About Zerus","children":null},
                {"id":"separator-1","kind":"predefined","caption":"","children":null},
                {"id":"services","kind":"predefined","caption":"Services","children":null},
                {"id":"separator-2","kind":"predefined","caption":"","children":null},
                {"id":"hide","kind":"predefined","caption":"Hide Zerus","children":null},
                {"id":"hide-others","kind":"predefined","caption":"Hide Others","children":null},
                {"id":"show-all","kind":"predefined","caption":"Show All","children":null},
                {"id":"separator-3","kind":"predefined","caption":"","children":null},
                {"id":"quit","kind":"item","caption":"Quit Zerus","children":null}]},
            {"id":"edit","kind":"submenu","caption":"Edit","children":[
                {"id":"undo","kind":"predefined","caption":"Undo","children":null},
                {"id":"redo","kind":"predefined","caption":"Redo","children":null},
                {"id":"cut","kind":"predefined","caption":"Cut","children":null},
                {"id":"copy","kind":"predefined","caption":"Copy","children":null},
                {"id":"paste","kind":"predefined","caption":"Paste","children":null},
                {"id":"select-all","kind":"predefined","caption":"Select All","children":null}]},
            {"id":"window","kind":"submenu","caption":"Window","children":[]}
        ]);
        let expected = expected_menu_with_show_all_and_quit(&before, 7, "show-all", "quit")
            .expect("the Tauri 2.12.5 application menu has eight entries");
        assert_eq!(expected, after);
        assert!(expected_menu_with_show_all_and_quit(&before, 50, "show-all", "quit").is_none());
        let mut old_order = after.clone();
        old_order[0]["children"].as_array_mut().unwrap().swap(6, 7);
        assert_ne!(expected, old_order);
        let mut missing = after.clone();
        missing[1]["children"].as_array_mut().unwrap().remove(2);
        assert_ne!(expected, missing);
        let mut changed_quit = after;
        changed_quit[0]["children"][8]["caption"] = "Quit something else".into();
        assert_ne!(expected, changed_quit);
    }

    #[test]
    fn a_failed_worker_can_release_only_its_own_quit() {
        let flush = ExitFlush::default();
        let nonce = waiting(&flush, 4);
        assert!(!flush.release("foreign"));
        assert!(!flush.consume(Some(4)));
        assert!(flush.release(&nonce));
        assert!(!flush.release(&nonce));
        assert!(flush.consume(Some(4)));
    }

    #[test]
    fn a_stale_waiter_cannot_release_a_different_request() {
        let flush = ExitFlush::default();
        let nonce = waiting(&flush, 0);
        assert!(!flush.wait("stale"));
        assert!(!flush.consume(Some(0)));
        flush.acknowledge(&nonce, crate::MAIN_WINDOW);
        assert!(flush.wait(&nonce));
        assert!(flush.consume(Some(0)));
    }
}
