//! Automatic recovery's shared policy (`zerus_client_core::recovery_settings`):
//! a background service, as Qt's RecoverySync is. Once a second it follows
//! this computer's machine card and the peers; every 15 seconds, as Qt's
//! timer, it reads every machine. Nothing is read or written before the card
//! says whether this computer runs agents (or, without one for 30 s, Qt's
//! default that it does), and a replay never drives the CLI. The newest
//! policy is copied to machines behind it; each answer sends
//! `recovery-changed` with the page's view. Calls run on their own threads.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{json, Map, Value};
use tauri::{AppHandle, Emitter, Manager, State};
use zerus_client_core::recovery_settings::{
    self, assumed_card, form, policy, tick, Call, Effect, Form, RecoverySync, REFRESH,
    SHARED_POLICY_KEY,
};
use zerus_client_core::{default_hgs, ComputerId};

use crate::commands::AppState;
use crate::settings::Store;

/// How often the service looks at the fleet.
const FOLLOW: Duration = Duration::from_secs(1);
const SERVICE_UNAVAILABLE: &str =
    zerus_client_core::source!("Recovery settings are unavailable from the desktop service.");

/// The service's state.
pub struct Recovery(Arc<Inner>);

struct Inner {
    sync: Mutex<RecoverySync>,
    /// Orders the stored policy's writes, which run outside `sync`.
    persist: Mutex<()>,
    /// A replay without an explicit CLI: nothing is read or written.
    blocked: bool,
    service_mode: bool,
    projection: Mutex<Option<zerus_client_core::maintenance::RecoveryProjection>>,
}

impl Inner {
    fn sync(&self) -> MutexGuard<'_, RecoverySync> {
        self.sync.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn view(&self) -> Value {
        if !self.service_mode {
            return view(&self.sync());
        }
        let projected = self
            .projection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(projected) = projected.as_ref() else {
            return json!({"status":"","error":SERVICE_UNAVAILABLE,
                "busy":false,"can_save":false,"known":false,"has_machines":false,"has_policy":false,"saves":0});
        };
        let current = projected.ready && projected.current_origin;
        json!({"status":projected.status,"error":projected.error,"busy":current && projected.busy,
            "can_save":current && projected.can_save,"known":current && projected.known,"has_machines":current && projected.has_machines,
            "has_policy":projected.has_policy,"saves":projected.saves})
        // This only enables the form. The authenticated native sender and
        // service mutation boundary still admit the actual Save intent.
    }
    fn envelope(&self) -> Map<String, Value> {
        if !self.service_mode {
            return self.sync().envelope().clone();
        }
        self.projection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|p| p.envelope.clone())
            .unwrap_or_default()
    }

    /// Stores the newest policy known now. Writes take turns, and each
    /// reads the policy once it has its turn, so a slower thread never
    /// stores an older one; the store and its listeners run outside `sync`.
    fn persist(&self, store: &Store) {
        let _turn = self.persist.lock().unwrap_or_else(PoisonError::into_inner);
        let newest = Value::Object(self.sync().envelope().clone()).to_string();
        let _ = store.0.set(SHARED_POLICY_KEY, json!(newest));
    }
}

impl Recovery {
    pub(crate) fn native_configuration(
        &self,
    ) -> zerus_client_core::maintenance::RecoveryConfiguration {
        let mut base = self.0.envelope();
        if base.is_empty() {
            // Historical cache only: the actor establishes machine currentness.
            base = self.0.sync().envelope().clone();
        }
        zerus_client_core::maintenance::RecoveryConfiguration { policy: None, base }
    }
}

/// What the page shows of the service.
fn view(sync: &RecoverySync) -> Value {
    json!({
        "status": sync.status(),
        "error": sync.error(),
        "busy": sync.busy(),
        "can_save": sync.can_save(),
        "known": sync.known(),
        "has_machines": sync.has_machines(),
        "has_policy": !sync.envelope().is_empty(),
        "saves": sync.saves(),
    })
}

/// Whether the service may read and write machines: never in a replay
/// without an explicit CLI (actions::blocked), as for read marks.
fn allowed(blocked: bool) -> Result<(), String> {
    if blocked {
        Err("Automatic recovery is not synchronized in a replay.".into())
    } else {
        Ok(())
    }
}

/// Starts the service from the newest policy kept from before. Its first
/// look at the fleet comes a second later, and its first timed read 15 s
/// later, as Qt's timer; a replay starts nothing.
pub fn start(app: &AppHandle) -> Recovery {
    let stored = app.state::<Store>().0.text(SHARED_POLICY_KEY);
    let inner = Arc::new(Inner {
        sync: Mutex::new(RecoverySync::new(&stored)),
        persist: Mutex::new(()),
        blocked: crate::actions::blocked(),
        service_mode: crate::desktop_service::mode(),
        projection: Mutex::new(None),
    });
    if !inner.blocked && !inner.service_mode {
        let (handle, service) = (app.clone(), Arc::clone(&inner));
        let periods = (REFRESH.as_secs() / FOLLOW.as_secs()).max(1);
        let started = std::thread::Builder::new()
            .name("recovery-sync".into())
            .spawn(move || {
                let mut second = 0u64;
                let mut seen = false;
                loop {
                    std::thread::sleep(FOLLOW);
                    second += 1;
                    let waited = FOLLOW * u32::try_from(second).unwrap_or(u32::MAX);
                    seen |= step(
                        &handle,
                        &service,
                        waited,
                        seen,
                        second.is_multiple_of(periods),
                    );
                }
            });
        if let Err(error) = started {
            eprintln!("zerus-client: automatic recovery settings: {error}");
        }
    }
    Recovery(inner)
}

/// Apply a read-only service projection to the existing Recovery view. The
/// separate recovery executor and its policy persistence are untouched.
pub(crate) fn service_projection(
    app: &AppHandle,
    value: Option<&zerus_client_core::maintenance::RecoveryProjection>,
) {
    let recovery = app.state::<Recovery>();
    if !recovery.0.service_mode {
        return;
    }
    *recovery
        .0
        .projection
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = value.cloned();
    let _ = app.emit("recovery-changed", recovery.0.view());
}

/// One look at the fleet `waited` after the service started: this
/// computer's card and the peers, and a read of every machine when `due`.
/// Returns whether the real card was there (`seen`: it has been before).
fn step(app: &AppHandle, inner: &Arc<Inner>, waited: Duration, seen: bool, due: bool) -> bool {
    let state = app.state::<AppState>();
    let real = state.pump().machine(&ComputerId::local());
    let present = real.is_some();
    let card = assumed_card(real, waited, seen);
    let peers = state.peers().map(|peers| peers.into_iter().collect());
    let (calls, persist) = {
        let mut sync = inner.sync();
        settle(tick(&mut sync, card.as_ref(), peers, due))
    };
    if persist {
        inner.persist(&app.state::<Store>());
    }
    if due || !calls.is_empty() {
        changed(app, inner);
    }
    spawn(app, inner, calls);
    present
}

/// The calls to run, and whether the stored policy is to be written (with
/// [`Inner::persist`], once the service's lock is released). A Persist's own
/// text is not what is stored: the newest policy at the time of the write is.
fn settle(effects: Vec<Effect>) -> (Vec<Call>, bool) {
    let mut calls = Vec::new();
    let mut persist = false;
    for effect in effects {
        match effect {
            Effect::Call(call) => calls.push(call),
            Effect::Persist(_) => persist = true,
        }
    }
    (calls, persist)
}

fn changed(app: &AppHandle, inner: &Arc<Inner>) {
    let view = view(&inner.sync());
    let _ = app.emit("recovery-changed", view);
}

/// Feeds one answer and runs what follows.
fn answered(
    app: &AppHandle,
    inner: &Arc<Inner>,
    id: u64,
    reply: Result<Map<String, Value>, String>,
) {
    let (calls, persist) = settle(inner.sync().answered(id, reply));
    if persist {
        inner.persist(&app.state::<Store>());
    }
    spawn(app, inner, calls);
    changed(app, inner);
}

/// Runs each call on a thread of its own; a thread that cannot start
/// answers its call as failed, so nothing stays pending for good.
fn spawn(app: &AppHandle, inner: &Arc<Inner>, calls: Vec<Call>) {
    for call in calls {
        let id = call.id;
        let (handle, service) = (app.clone(), Arc::clone(inner));
        let started = std::thread::Builder::new()
            .name("recovery-call".into())
            .spawn(move || {
                let reply = recovery_settings::call(&default_hgs(), &call);
                answered(&handle, &service, call.id, reply);
            });
        if let Err(error) = started {
            answered(app, inner, id, Err(format!("Could not start hgs: {error}")));
        }
    }
}

/// The page's view.
#[tauri::command]
pub fn recovery_view(recovery: State<'_, Recovery>) -> Value {
    recovery.0.view()
}

/// The form for the newest policy and that policy (`base`), which the page
/// keeps with the form and hands back to Save (RecoverySettings::load);
/// both null before any policy.
#[tauri::command]
pub fn recovery_load(recovery: State<'_, Recovery>) -> Value {
    let envelope = recovery.0.envelope();
    let loaded = form(&envelope);
    let base = loaded.as_ref().map(|_| Value::Object(envelope));
    json!({ "form": loaded, "base": base })
}

/// Save recovery settings: the form's delays checked as Qt checks them,
/// then the policy sent on the base the form was loaded with. Answers the
/// view, Qt's text when the form is refused, and whether it was sent.
#[tauri::command(async)]
pub fn recovery_save(
    app: AppHandle,
    recovery: State<'_, Recovery>,
    form: Form,
    base: Map<String, Value>,
) -> Result<Value, String> {
    allowed(recovery.0.blocked)?;
    if recovery.0.service_mode {
        return Err(SERVICE_UNAVAILABLE.into());
    }
    let (calls, persist, message, view) = {
        let mut sync = recovery.0.sync();
        let (effects, message) = match policy(&base, &form) {
            Err(message) => (Vec::new(), Some(message)),
            Ok(policy) => (sync.save(policy, &base), None),
        };
        let (calls, persist) = settle(effects);
        (calls, persist, message, view(&sync))
    };
    if persist {
        recovery.0.persist(&app.state::<Store>());
    }
    let sent = !calls.is_empty();
    spawn(&app, &recovery.0, calls);
    Ok(json!({ "view": view, "message": message, "sent": sent }))
}

/// Reload, and opening the section: every machine read again.
#[tauri::command(async)]
pub fn recovery_reload(app: AppHandle, recovery: State<'_, Recovery>) -> Result<Value, String> {
    allowed(recovery.0.blocked)?;
    if recovery.0.service_mode {
        return Ok(recovery.0.view());
    }
    let (calls, persist, view) = {
        let mut sync = recovery.0.sync();
        let (calls, persist) = settle(sync.refresh());
        (calls, persist, view(&sync))
    };
    if persist {
        recovery.0.persist(&app.state::<Store>());
    }
    spawn(&app, &recovery.0, calls);
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replay_neither_reloads_nor_saves() {
        assert!(allowed(true).is_err());
        assert!(allowed(false).is_ok());
    }

    #[test]
    fn the_stored_policy_is_the_newest_at_the_time_it_is_written() {
        let dir =
            std::env::temp_dir().join(format!("zerus-recovery-settle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store(zerus_client_core::settings::Settings::open(
            dir.join("settings.json"),
            None,
        ));
        let newest = json!({"order": [9, "w"], "policy": {"revision": 9}});
        let inner = Inner {
            sync: Mutex::new(RecoverySync::new(&newest.to_string())),
            persist: Mutex::new(()),
            blocked: false,
            service_mode: false,
            projection: Mutex::new(None),
        };
        // A slower thread's Persist carries an older policy; the newest is kept.
        let older = json!({"order": [8, "w"], "policy": {"revision": 8}}).to_string();
        let (calls, persist) = settle(vec![Effect::Persist(older)]);
        assert!(calls.is_empty() && persist);
        inner.persist(&store);
        assert_eq!(store.0.text(SHARED_POLICY_KEY), newest.to_string());
        assert!(store.0.flush_within(Duration::from_secs(5)));
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn service_projection_is_read_only_and_missing_source_never_restarts_local_sync() {
        let inner = Inner {
            sync: Mutex::new(RecoverySync::new("")),
            persist: Mutex::new(()),
            blocked: false,
            service_mode: true,
            projection: Mutex::new(None),
        };
        assert_eq!(inner.view()["known"], false);
        assert_eq!(inner.view()["can_save"], false);
        let projection = zerus_client_core::maintenance::RecoveryProjection {
            ready: true,
            current_origin: true,
            known: true,
            has_machines: true,
            has_policy: true,
            can_save: true,
            envelope: json!({"order":[3,"fixture"],"policy":{"enabled":false}})
                .as_object()
                .unwrap()
                .clone(),
            ..zerus_client_core::maintenance::RecoveryProjection::default()
        };
        *inner.projection.lock().unwrap() = Some(projection.clone());
        assert_eq!(inner.view()["known"], true);
        assert_eq!(
            inner.view()["can_save"],
            true,
            "readiness enables the form, not the native mutation boundary"
        );
        assert_eq!(inner.envelope(), projection.envelope);
        assert!(
            !inner.sync().known(),
            "service projection never drives the local sync engine"
        );
        *inner.projection.lock().unwrap() = None;
        assert_eq!(inner.view()["known"], false);
        assert!(inner.envelope().is_empty());
    }
    #[test]
    fn native_configuration_preserves_the_policy_cache_without_replaying_an_edit() {
        let cached = json!({"order":[3,"fixture"],"policy":{"enabled":false}});
        let recovery = Recovery(Arc::new(Inner {
            sync: Mutex::new(RecoverySync::new(&cached.to_string())),
            persist: Mutex::new(()),
            blocked: false,
            service_mode: true,
            projection: Mutex::new(None),
        }));
        let seed = recovery.native_configuration();
        assert!(seed.policy.is_none());
        assert_eq!(seed.base, cached.as_object().unwrap().clone());
        let current = json!({"order":[4,"fixture"],"policy":{"enabled":true}});
        *recovery.0.projection.lock().unwrap() =
            Some(zerus_client_core::maintenance::RecoveryProjection {
                envelope: current.as_object().unwrap().clone(),
                ..zerus_client_core::maintenance::RecoveryProjection::default()
            });
        assert_eq!(
            recovery.native_configuration().base,
            current.as_object().unwrap().clone()
        );
        assert!(
            !recovery.0.sync().known(),
            "historical data never establishes a current machine"
        );
        assert_eq!(recovery.0.view()["can_save"], false);
    }
}
