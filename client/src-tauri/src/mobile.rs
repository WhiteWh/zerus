//! Settings > Mobile connection: one status read, one bounded phones sweep
//! and one notification-timing call at a time, with "mobile-changed" answers.
//! The child modules share one effects gate for bounded one-use phone,
//! computer-invitation, workspace and connector lifecycle plans. Existing CLI commands retain
//! connector/service ownership and saved enrollment recovery. A replay never
//! drives the CLI.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};
use zerus_client_core::default_hgs;
use zerus_client_core::mobile::page::{Delivery, Page, View};
use zerus_client_core::mobile::{self, PhoneList, Workspace};

use crate::organization::Organization;

mod phones;
pub use phones::*;
mod computers;
pub use computers::*;
mod enrollment;
pub use enrollment::*;
mod connector_actions;
pub use connector_actions::*;
mod legacy;
pub use legacy::*;

const REPLAY: &str = "The mobile connection is not read in a replay.";
/// Computers whose phones are read at once.
const WORKERS: usize = 8;

/// One computer's phones: its list, none, or why it was not read.
type Listed = Result<Option<PhoneList>, String>;
type Call<T> = Box<dyn Fn() -> Result<T, String> + Send + Sync>;
/// One call with its input.
type CallWith<I, T> = Box<dyn Fn(&I) -> Result<T, String> + Send + Sync>;

/// The calls the page makes; each blocks.
pub struct Calls {
    pub status: Call<Workspace>,
    pub hosts: Call<Vec<String>>,
    pub phones: CallWith<str, Option<PhoneList>>,
    pub delivery: CallWith<Delivery, String>,
}

fn hgs_calls() -> Calls {
    Calls {
        status: Box::new(|| {
            let (connector, config) = workspace_runtime();
            mobile::read_status(&connector, &config)
        }),
        hosts: Box::new(|| mobile::hosts(&default_hgs())),
        phones: Box::new(|host| mobile::phones(&default_hgs(), host)),
        delivery: Box::new(|call| match call {
            Delivery::Get => mobile::delivery(&default_hgs(), None),
            Delivery::Set(value) => mobile::delivery(&default_hgs(), Some(value)),
        }),
    }
}

fn workspace_runtime() -> (std::path::PathBuf, std::path::PathBuf) {
    let client_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf));
    (
        mobile::connector_program(&default_hgs(), client_dir.as_deref()),
        mobile::config_path(),
    )
}

pub struct Mobile(Arc<Inner>);

struct Inner {
    page: Mutex<Page>,
    phone_plans: Mutex<phones::Plans>,
    computer_plans: Mutex<computers::Plans>,
    enrollment_plans: Mutex<enrollment::Plans>,
    connector_plans: Mutex<connector_actions::Plans>,
    legacy_plan: Mutex<Option<legacy::Plan>>,
    effects: std::sync::atomic::AtomicBool,
    /// The page's clock: monotonic, so a changed system time neither names
    /// nor hides the sync service early.
    started: Instant,
    blocked: bool,
    calls: Calls,
    notify: Box<dyn Fn() + Send + Sync>,
}

pub fn start(app: &AppHandle) -> Mobile {
    let handle = app.clone();
    Mobile(Arc::new(Inner::new(
        crate::actions::blocked(),
        hgs_calls(),
        Box::new(move || {
            let _ = handle.emit("mobile-changed", ());
        }),
    )))
}

/// Runs `work` on a thread of its own; false when it could not start.
fn spawn(name: &str, work: impl FnOnce() + Send + 'static) -> bool {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(work)
        .is_ok()
}

impl Inner {
    fn new(blocked: bool, calls: Calls, notify: Box<dyn Fn() + Send + Sync>) -> Inner {
        Inner {
            page: Mutex::new(Page::new()),
            phone_plans: Mutex::new(phones::Plans::default()),
            computer_plans: Mutex::new(computers::Plans::default()),
            enrollment_plans: Mutex::new(enrollment::Plans::default()),
            connector_plans: Mutex::new(connector_actions::Plans::default()),
            legacy_plan: Mutex::new(None),
            effects: std::sync::atomic::AtomicBool::new(false),
            started: Instant::now(),
            blocked,
            calls,
            notify,
        }
    }

    fn now_ms(&self) -> i64 {
        i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX)
    }

    fn page(&self) -> MutexGuard<'_, Page> {
        self.page.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The page, with the latest swarm read's timing and notices.
    fn view(&self, snapshot: Option<&Value>) -> View {
        let mut page = self.page();
        if let Some(snapshot) = snapshot {
            page.swarm_snapshot(snapshot, self.now_ms());
        }
        page.view()
    }

    /// Opening the section: its status, its phones (merged into a sweep
    /// still running) and the timing, each unless one runs.
    fn open(self: &Arc<Self>) {
        if self.blocked {
            self.page().refuse(REPLAY);
            return;
        }
        self.workspace();
        self.phones();
        let call = self.page().read_delivery();
        if let Some(call) = call {
            self.delivery(call);
        }
    }

    fn workspace(self: &Arc<Self>) {
        if self.blocked {
            self.page().refuse(REPLAY);
            return;
        }
        if !self.page().read_workspace() {
            return;
        }
        let inner = Arc::clone(self);
        let started = spawn("mobile-status", move || {
            let answer = (inner.calls.status)();
            let changed = inner.page().workspace_answered(answer);
            (inner.notify)();
            // A new connection lists other phones (Qt's connectionChanged).
            if changed {
                inner.phones();
            }
        });
        if !started {
            self.page()
                .workspace_answered(Err("Workspace status is unconfirmed".into()));
        }
    }

    fn phones(self: &Arc<Self>) {
        if self.blocked {
            self.page().refuse(REPLAY);
            return;
        }
        if !self.page().read_phones() {
            return;
        }
        let inner = Arc::clone(self);
        let started = spawn("mobile-phones", move || loop {
            let phones = inner.sweep();
            let again = inner.page().phones_answered(phones);
            (inner.notify)();
            if !again {
                break;
            }
        });
        if !started {
            let failed = mobile::phones_view(Vec::new(), Some("could not start".into()), false);
            self.page().phones_unstarted(failed);
        }
    }

    /// PhonePairing's refresh: the machine list, then every enabled
    /// computer's phones, at most [`WORKERS`] at once.
    fn sweep(&self) -> mobile::Phones {
        let (hosts, machines) = match (self.calls.hosts)() {
            Ok(hosts) => (hosts, None),
            Err(error) => (vec![String::new()], Some(error)),
        };
        let next = AtomicUsize::new(0);
        let answers: Vec<Mutex<Option<Listed>>> = hosts.iter().map(|_| Mutex::new(None)).collect();
        std::thread::scope(|scope| {
            for _ in 0..WORKERS.min(hosts.len()) {
                scope.spawn(|| loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(host) = hosts.get(index) else {
                        break;
                    };
                    let answer = (self.calls.phones)(host);
                    *answers[index]
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner) = Some(answer);
                });
            }
        });
        let read = hosts
            .into_iter()
            .zip(answers)
            .map(|(host, answer)| {
                let answer = answer
                    .into_inner()
                    .unwrap_or_else(PoisonError::into_inner)
                    .unwrap_or_else(|| Err("Invalid phone management response".into()));
                (host, answer)
            })
            .collect();
        mobile::phones_view(read, machines, cfg!(windows))
    }

    fn delivery(self: &Arc<Self>, call: Delivery) {
        let inner = Arc::clone(self);
        let started = spawn("mobile-delivery", move || {
            let mut call = call;
            loop {
                let answer = (inner.calls.delivery)(&call);
                let next = inner.page().delivery_answered(answer, inner.now_ms());
                (inner.notify)();
                match next {
                    Some(again) => call = again,
                    None => break,
                }
            }
        });
        if !started {
            let now = self.now_ms();
            self.page().delivery_unstarted(now);
        }
    }

    fn set_delivery(self: &Arc<Self>, value: &str) {
        if self.blocked {
            self.page().refuse(REPLAY);
            return;
        }
        let call = self.page().set_delivery(value);
        if let Some(call) = call {
            self.delivery(call);
        }
    }
}

fn snapshot(app: &AppHandle) -> Option<Arc<Value>> {
    app.try_state::<Organization>()?.swarm_snapshot()
}

/// The page as it stands, with the latest swarm read's notices.
#[tauri::command(async)]
pub fn mobile_view(app: AppHandle, mobile: State<'_, Mobile>) -> View {
    mobile.0.view(snapshot(&app).as_deref())
}

/// Opening the section.
#[tauri::command(async)]
pub fn mobile_open(app: AppHandle, mobile: State<'_, Mobile>) -> View {
    mobile.0.open();
    mobile.0.view(snapshot(&app).as_deref())
}

/// Refresh status.
#[tauri::command(async)]
pub fn mobile_refresh_workspace(app: AppHandle, mobile: State<'_, Mobile>) -> View {
    mobile.0.workspace();
    mobile.0.view(snapshot(&app).as_deref())
}

/// The phones' Refresh.
#[tauri::command(async)]
pub fn mobile_refresh_phones(app: AppHandle, mobile: State<'_, Mobile>) -> View {
    mobile.0.phones();
    mobile.0.view(snapshot(&app).as_deref())
}

/// The shared timing changes to one of Qt's values.
#[tauri::command(async)]
pub fn mobile_delivery_set(app: AppHandle, mobile: State<'_, Mobile>, value: String) -> View {
    mobile.0.set_delivery(&value);
    mobile.0.view(snapshot(&app).as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::time::Duration;

    pub(super) fn service_calls_for_plans() -> Calls {
        Calls {
            status: Box::new(|| Err("unused".into())),
            hosts: Box::new(|| Ok(vec![])),
            phones: Box::new(|_| Ok(None)),
            delivery: Box::new(|_| Err("unused".into())),
        }
    }

    /// Counts each kind of call; the calls wait while `hold` is set.
    #[derive(Default)]
    struct Log {
        status: AtomicUsize,
        hosts: AtomicUsize,
        phones: AtomicUsize,
        delivery: Mutex<Vec<Delivery>>,
        running: AtomicUsize,
        most: AtomicUsize,
        hold: AtomicBool,
        revision: Mutex<String>,
        save_fails: AtomicBool,
    }

    fn wait(log: &Log) {
        while log.hold.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn service(blocked: bool, hosts: usize) -> (Arc<Inner>, Arc<Log>, mpsc::Receiver<()>) {
        let log = Arc::new(Log::default());
        let (tell, told) = mpsc::channel();
        let (a, b, c, d) = (
            Arc::clone(&log),
            Arc::clone(&log),
            Arc::clone(&log),
            Arc::clone(&log),
        );
        let calls = Calls {
            status: Box::new(move || {
                a.status.fetch_add(1, Ordering::SeqCst);
                wait(&a);
                let revision = a.revision.lock().unwrap().clone();
                let value = serde_json::json!({"state": "unconfigured", "encrypted": false, "config_revision": revision});
                Ok(mobile::workspace(value.as_object().unwrap()))
            }),
            hosts: Box::new(move || {
                b.hosts.fetch_add(1, Ordering::SeqCst);
                Ok(std::iter::once(String::new())
                    .chain((1..hosts).map(|n| format!("peer-{n}")))
                    .collect())
            }),
            phones: Box::new(move |_| {
                c.phones.fetch_add(1, Ordering::SeqCst);
                let now = c.running.fetch_add(1, Ordering::SeqCst) + 1;
                c.most.fetch_max(now, Ordering::SeqCst);
                wait(&c);
                std::thread::sleep(Duration::from_millis(1));
                c.running.fetch_sub(1, Ordering::SeqCst);
                Ok(None)
            }),
            delivery: Box::new(move |call| {
                d.delivery.lock().unwrap().push(call.clone());
                wait(&d);
                match call {
                    Delivery::Set(_) if d.save_fails.load(Ordering::SeqCst) => {
                        Err("timed out".into())
                    }
                    Delivery::Set(value) => Ok(value.clone()),
                    Delivery::Get => Ok("away_5".into()),
                }
            }),
        };
        let inner = Arc::new(Inner::new(
            blocked,
            calls,
            Box::new(move || {
                let _ = tell.send(());
            }),
        ));
        (inner, log, told)
    }

    /// Waits until nothing runs.
    fn settled(inner: &Inner) -> View {
        for _ in 0..5000 {
            let view = inner.view(None);
            if !view.workspace_busy && !view.phones_busy && !view.delivery_busy {
                return view;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("still busy");
    }

    #[test]
    fn a_replay_runs_nothing_and_says_so() {
        let (inner, log, _) = service(true, 3);
        inner.open();
        inner.workspace();
        inner.phones();
        inner.set_delivery("away");
        let view = inner.view(None);
        assert_eq!(view.workspace_error.as_deref(), Some(REPLAY));
        assert_eq!(view.phones.unwrap().lines, [REPLAY]);
        assert_eq!(view.delivery_status, REPLAY);
        assert_eq!(log.status.load(Ordering::SeqCst), 0);
        assert_eq!(log.hosts.load(Ordering::SeqCst), 0);
        assert!(log.delivery.lock().unwrap().is_empty());
    }

    #[test]
    fn opening_the_section_again_while_it_reads_starts_nothing_new() {
        let (inner, log, _) = service(false, 3);
        log.hold.store(true, Ordering::SeqCst);
        for _ in 0..20 {
            inner.open();
            inner.workspace();
            inner.phones();
        }
        let view = inner.view(None);
        assert!(view.workspace_busy && view.phones_busy && view.delivery_busy);
        log.hold.store(false, Ordering::SeqCst);
        settled(&inner);
        // One status read and one timing read; the sweep ran once more for
        // all the refreshes asked meanwhile, as Qt merges them.
        assert_eq!(log.status.load(Ordering::SeqCst), 1);
        assert_eq!(*log.delivery.lock().unwrap(), [Delivery::Get]);
        assert_eq!(log.hosts.load(Ordering::SeqCst), 2);
        assert_eq!(log.phones.load(Ordering::SeqCst), 6);
    }

    #[test]
    fn every_computer_is_read_at_most_eight_at_once() {
        let (inner, log, _) = service(false, 40);
        inner.phones();
        let view = settled(&inner);
        assert_eq!(log.phones.load(Ordering::SeqCst), 40);
        assert!(log.most.load(Ordering::SeqCst) <= WORKERS);
        assert_eq!(view.phones.unwrap().lists.len(), 0);
    }

    #[test]
    fn a_failed_save_reads_the_value_again() {
        let (inner, log, _) = service(false, 1);
        inner.open();
        settled(&inner);
        log.save_fails.store(true, Ordering::SeqCst);
        inner.set_delivery("away_10");
        let view = settled(&inner);
        assert_eq!(
            *log.delivery.lock().unwrap(),
            [
                Delivery::Get,
                Delivery::Set("away_10".into()),
                Delivery::Get
            ]
        );
        assert_eq!(view.delivery.as_deref(), Some("away_5"));
        assert_eq!(view.delivery_status, "Could not save: timed out");
        // A value Qt does not offer is never sent.
        inner.set_delivery("sometimes");
        assert_eq!(log.delivery.lock().unwrap().len(), 3);
    }

    #[test]
    fn a_new_connection_reads_the_phones_again() {
        let (inner, log, _) = service(false, 1);
        *log.revision.lock().unwrap() = "a".repeat(64);
        inner.open();
        settled(&inner);
        let sweeps = log.hosts.load(Ordering::SeqCst);
        inner.workspace();
        settled(&inner);
        assert_eq!(log.hosts.load(Ordering::SeqCst), sweeps);
        *log.revision.lock().unwrap() = "b".repeat(64);
        inner.workspace();
        settled(&inner);
        for _ in 0..500 {
            if log.hosts.load(Ordering::SeqCst) > sweeps {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        settled(&inner);
        assert_eq!(log.hosts.load(Ordering::SeqCst), sweeps + 1);
    }
}
