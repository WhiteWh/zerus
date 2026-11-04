//! Independent of the selected session/window, as Qt AutomaticApprovals.
//! Discovery retains a cursor, two jobs and at most 4096 cooldown keys. Native
//! input still uses the one existing Actions::dispatch_as Answer lane.
use crate::{
    actions::Actions,
    pump::{ApprovalCursor, Pump},
};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use zerus_client_core::{
    approvals::{self, automatic as policy, Operation},
    settings::Settings,
    Action, Outcome,
};
type Inspect = Arc<dyn Fn(&policy::Candidate) -> Result<Value, String> + Send + Sync>;
type Journal = Arc<dyn Fn(Operation, &Value) -> Result<Value, String> + Send + Sync>;
type Dispatch =
    Arc<dyn Fn(&policy::Candidate, Action, String) -> Result<Outcome, String> + Send + Sync>;
struct Calls {
    current: Arc<dyn Fn(&policy::Candidate) -> bool + Send + Sync>,
    inspect: Inspect,
    journal: Journal,
    dispatch: Dispatch,
}
/// No queue or replay path. Every begin is local and durable; an existing
/// reservation (including prepared/unknown/cancelled) ends this attempt.
/// True means the journal confirmed a finish, so an expired journal error can clear.
fn attempt(calls: &Calls, target: &policy::Candidate) -> Result<bool, String> {
    if !(calls.current)(target) {
        return Ok(false);
    }
    let Ok(details) = (calls.inspect)(target) else {
        return Ok(false);
    };
    if !(calls.current)(target) {
        return Ok(false);
    }
    let Some(question) = policy::question(target, &details) else {
        return Ok(false);
    };
    let host = if target.target.computer.is_empty() {
        details["machine"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("local")
    } else {
        &target.target.computer
    };
    let reply = (calls.journal)(
        Operation::Begin,
        &json!({"host":host,"session":target.target.name,"provider":target.provider,"question":question}),
    )?;
    let Some(reservation) =
        policy::reserved(&reply, &target.provider, &question).map_err(str::to_owned)?
    else {
        return Ok(false);
    };
    let (status, detail) = if !(calls.current)(target) {
        ("cancelled", policy::CANCELLED.into())
    } else {
        let action = Action::Answer {
            question,
            answers: reservation.answers,
        };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (calls.dispatch)(target, action, reservation.request.clone())
        })) {
            Ok(Ok(Outcome::Done { .. })) => ("approved", String::new()),
            Ok(Ok(Outcome::Failed { detail })) => ("failed", detail),
            Ok(Ok(Outcome::Uncertain { detail })) => ("unknown", detail),
            Ok(Err(_)) | Err(_) => (
                "unknown",
                "Answer delivery could not be confirmed. Check Activity or Terminal.".into(),
            ),
        }
    };
    let detail: String = detail.chars().take(2000).collect();
    let reply = (calls.journal)(
        Operation::Finish,
        &json!({"id":reservation.id,"request_id":reservation.request,"status":status,"detail":detail}),
    )?;
    if reply["id"] != reservation.id || reply["status"] != status {
        return Err("The approval journal did not confirm the outcome.".into());
    }
    Ok(true)
}
#[derive(Default)]
struct Scan {
    cursor: ApprovalCursor,
    active: HashSet<String>,
    recent: HashMap<String, Instant>,
    order: VecDeque<String>,
    paused: Option<Instant>,
}
struct Shared {
    pump: Pump,
    settings: Settings,
    calls: Calls,
    scan: Mutex<Scan>,
    stopped: Arc<AtomicBool>,
}
pub(super) struct Worker(Arc<Shared>);
impl Drop for Worker {
    fn drop(&mut self) {
        self.0.stopped.store(true, Ordering::Release);
    }
}
impl Worker {
    pub(super) fn start(
        pump: Pump,
        actions: Actions,
        settings: Settings,
        supported: Arc<AtomicBool>,
    ) -> Self {
        // A replay is evidence, never a background permission source, even
        // when its explicit test CLI allows individual manual smoke actions.
        let blocked = crate::actions::blocked()
            || std::env::var_os("ZERUS_CLIENT_REPLAY").is_some_and(|value| !value.is_empty());
        let stopped = Arc::new(AtomicBool::new(false));
        let current = {
            let pump = pump.clone();
            let settings = settings.clone();
            let stopped = stopped.clone();
            let actions = actions.clone();
            Arc::new(move |target: &policy::Candidate| {
                !blocked
                    && !stopped.load(Ordering::Acquire)
                    && settings.flag(policy::ENABLED)
                    && pump.approval_current(target)
                    && actions
                        .session_write(&target.target.computer, &target.target.name, || {
                            (
                                pump.row(&target.target.computer, &target.catalog_key),
                                pump.online(&target.target.computer),
                            )
                        })
                        .is_none()
            })
        };
        let calls = Calls {
            current,
            inspect: Arc::new(|target| {
                let hgs = zerus_client_core::default_hgs();
                let inv = zerus_client_core::details::details_invocation(&target.target, None);
                crate::composer::send::inspection(
                    &hgs,
                    zerus_client_core::run_limited(&hgs, &inv, &[], 8 << 20),
                )
            }),
            journal: Arc::new(move |operation, value| {
                let mut known = supported.load(Ordering::Acquire);
                let reply = approvals::call(
                    &zerus_client_core::default_hgs(),
                    operation,
                    value,
                    &mut known,
                )
                .map(Value::Object);
                if known {
                    supported.store(true, Ordering::Release);
                }
                reply
            }),
            dispatch: Arc::new(move |target, action, id| {
                tauri::async_runtime::block_on(crate::actions::dispatch_as(
                    &actions,
                    target.target.clone(),
                    action,
                    id,
                ))
            }),
        };
        let shared = Arc::new(Shared {
            pump,
            settings,
            calls,
            scan: Mutex::new(Scan::default()),
            stopped,
        });
        let weak = Arc::downgrade(&shared);
        std::thread::Builder::new()
            .name("zerus-automatic-approvals".into())
            .spawn(move || {
                while let Some(shared) = weak.upgrade() {
                    if shared.stopped.load(Ordering::Acquire) {
                        break;
                    }
                    if !blocked && shared.settings.flag(policy::ENABLED) {
                        tick(&shared);
                    }
                    drop(shared);
                    std::thread::sleep(Duration::from_millis(250));
                }
            })
            .expect("the automatic approval worker starts");
        Self(shared)
    }
}
fn tick(shared: &Arc<Shared>) {
    let targets = {
        let mut scan = shared
            .scan
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        if scan.active.len() >= 2 || scan.paused.is_some_and(|until| until > now) {
            return;
        }
        while scan
            .order
            .front()
            .is_some_and(|key| scan.recent.get(key).is_none_or(|until| *until <= now))
        {
            let key = scan.order.pop_front().unwrap();
            scan.recent.remove(&key);
        }
        let Scan {
            cursor,
            active,
            recent,
            ..
        } = &mut *scan;
        let targets = shared
            .pump
            .approval_candidates(cursor, 2 - active.len(), |target| {
                !active.contains(&target.key) && !recent.contains_key(&target.key)
            });
        for target in &targets {
            active.insert(target.key.clone());
        }
        targets
    };
    for target in targets {
        let shared = shared.clone();
        let failed = shared.clone();
        let key = target.key.clone();
        if std::thread::Builder::new()
            .name("zerus-automatic-approval".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    attempt(&shared.calls, &target)
                }))
                .unwrap_or_else(|_| {
                    Err("Automatic approvals paused after an interrupted journal operation.".into())
                });
                released(&shared, &target.key, result);
            })
            .is_err()
        {
            released(
                &failed,
                &key,
                Err("The automatic approval worker could not start.".into()),
            );
        }
    }
}
fn released(shared: &Shared, key: &str, result: Result<bool, String>) {
    let mut scan = shared
        .scan
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = Instant::now();
    scan.active.remove(key);
    scan.order.retain(|old| old != key);
    if scan.order.len() >= 4096 {
        if let Some(old) = scan.order.pop_front() {
            scan.recent.remove(&old);
        }
    }
    scan.order.push_back(key.into());
    scan.recent
        .insert(key.into(), now + Duration::from_secs(10));
    match result {
        Err(error) => {
            scan.paused = Some(now + Duration::from_secs(60));
            drop(scan);
            let _: Result<_, _> = shared.settings.set(
                policy::ERROR,
                json!(error.chars().take(2000).collect::<String>()),
            );
        }
        Ok(true) if scan.paused.is_none_or(|until| until <= now) => {
            drop(scan);
            if shared.settings.get(policy::ERROR) != "" {
                let _ = shared.settings.set(policy::ERROR, json!(""));
            }
        }
        Ok(_) => {}
    }
}

#[cfg(test)]
mod tests;
