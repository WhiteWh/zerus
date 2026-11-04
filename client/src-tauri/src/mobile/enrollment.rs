//! Exact one-use enrollment intents; continuation only for a proven saved node.
use super::*;
use mobile::enrollment::{self as core, Answer, Mode, Request, Target};
use serde::Serialize;
use std::{collections::VecDeque, sync::atomic::AtomicBool, time::Duration};
const WAITING:&str="The connector is still finishing its current request. Your saved connection is kept; choose Continue connecting to check again.";
enum Intent {
    Enroll(Request),
    Check(bool),
}
struct Plan {
    target: Target,
    intent: Intent,
    computer: String,
}
#[derive(Clone)]
struct Continuation {
    handle: String,
    target: Target,
    until: Instant,
}
#[derive(Default)]
pub(super) struct Plans {
    pending: VecDeque<(String, Plan)>,
    continuation: Option<Continuation>,
}
#[derive(Serialize)]
pub struct Form {
    handle: String,
    target: Target,
    mode: Option<Mode>,
    changing: bool,
    legacy_local: bool,
}
#[derive(Serialize)]
pub struct Reply {
    notice: String,
    connected: bool,
    pending: bool,
    stopped_target: Option<Target>,
    continuation: Option<String>,
}
struct Active<'a>(&'a AtomicBool);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
fn computer_name() -> String {
    #[cfg(unix)]
    let computer = crate::instance::host_name();
    #[cfg(not(unix))]
    let computer = std::env::var("COMPUTERNAME").unwrap_or_default();
    let computer = computer.trim().chars().take(128).collect::<String>();
    if computer.is_empty() {
        "Computer".into()
    } else {
        computer
    }
}
impl Inner {
    fn enrollment_open(&self, requested: &Target, intent: Intent) -> Result<Form, String> {
        if self.blocked {
            return Err(REPLAY.into());
        }
        let view = self.page().view();
        if view.workspace_busy || view.workspace_error.is_some() {
            return Err(core::CHANGED.into());
        }
        let target = Target::capture(view.workspace.as_ref().ok_or(core::CHANGED)?)?;
        if target != *requested {
            return Err(core::CHANGED.into());
        }
        let (mode, legacy_local) = match &intent {
            Intent::Enroll(request) => {
                request.validate()?;
                if !target.can_enroll() {
                    return Err(core::CHANGED.into());
                }
                (Some(request.mode), false)
            }
            Intent::Check(local) => {
                if !target.can_check()
                    || (*local
                        && target.pending_state.as_deref() != Some("legacy-retirement-unconfirmed"))
                {
                    return Err(core::CHANGED.into());
                }
                (None, *local)
            }
        };
        let handle = uuid::Uuid::new_v4().to_string();
        let changing = target.changing();
        let mut plans = self
            .enrollment_plans
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while plans.pending.len() >= 8 {
            plans.pending.pop_front();
        }
        plans.pending.push_back((
            handle.clone(),
            Plan {
                target: target.clone(),
                intent,
                computer: computer_name(),
            },
        ));
        Ok(Form {
            handle,
            target,
            mode,
            changing,
            legacy_local,
        })
    }
    fn enrollment_active(&self) -> Result<Active<'_>, String> {
        if self.blocked {
            return Err(REPLAY.into());
        }
        if self.effects.swap(true, Ordering::AcqRel) {
            return Err("A mobile action is already running.".into());
        }
        Ok(Active(&self.effects))
    }
    fn enrollment_take(&self, handle: &str) -> Result<(Plan, Active<'_>), String> {
        let mut plans = self
            .enrollment_plans
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let at = plans
            .pending
            .iter()
            .position(|(h, _)| h == handle)
            .ok_or(core::CHANGED)?;
        let active = self.enrollment_active()?;
        let plan = plans.pending.remove(at).ok_or(core::CHANGED)?.1;
        plans.continuation = None;
        Ok((plan, active))
    }
    fn enrollment_close(&self, handle: &str) {
        let mut plans = self
            .enrollment_plans
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        plans.pending.retain(|(h, _)| h != handle);
        if plans
            .continuation
            .as_ref()
            .is_some_and(|c| c.handle == handle)
        {
            plans.continuation = None;
        }
    }
    fn enrollment_result(
        self: &Arc<Self>,
        answer: Answer,
        until: Instant,
    ) -> Result<Reply, String> {
        let target = match Target::capture(&answer.workspace) {
            Ok(target) => target,
            Err(error) => {
                self.enrollment_failed();
                return Err(error);
            }
        };
        let mut notice = answer.notice.to_owned();
        let mut handle = None;
        if answer.continue_node.as_deref() == target.pending_node_id.as_deref()
            && answer.continue_node.is_some()
        {
            if Instant::now() < until {
                let next = uuid::Uuid::new_v4().to_string();
                self.enrollment_plans
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .continuation = Some(Continuation {
                    handle: next.clone(),
                    target: target.clone(),
                    until,
                });
                handle = Some(next);
            } else {
                notice = WAITING.into();
            }
        }
        let pending = answer.workspace.pending.is_some();
        let stopped_target =
            (!pending && !answer.connected && answer.notice == core::DRAINING).then_some(target);
        if self.page().workspace_answered(Ok(answer.workspace)) {
            self.phones();
        }
        (self.notify)();
        Ok(Reply {
            notice,
            connected: answer.connected,
            pending,
            stopped_target,
            continuation: handle,
        })
    }
    fn enrollment_started(&self) -> Result<(), String> {
        if !self.page().read_workspace() {
            return Err(core::CHANGED.into());
        }
        (self.notify)();
        Ok(())
    }
    fn enrollment_failed(&self) {
        self.page()
            .workspace_answered(Err(mobile::WORKSPACE_UNCONFIRMED.into()));
        (self.notify)();
    }
}
#[tauri::command(async)]
pub fn mobile_workspace_open(
    mobile: State<'_, Mobile>,
    target: Target,
    request: Request,
) -> Result<Form, String> {
    mobile.0.enrollment_open(&target, Intent::Enroll(request))
}
#[tauri::command(async)]
pub fn mobile_workspace_check_open(
    mobile: State<'_, Mobile>,
    target: Target,
    allow_local: bool,
) -> Result<Form, String> {
    mobile
        .0
        .enrollment_open(&target, Intent::Check(allow_local))
}
#[tauri::command(async)]
pub fn mobile_workspace_close(mobile: State<'_, Mobile>, handle: String) {
    mobile.0.enrollment_close(&handle);
}
#[tauri::command(async)]
pub fn mobile_workspace_apply(mobile: State<'_, Mobile>, handle: String) -> Result<Reply, String> {
    let (plan, _active) = mobile.0.enrollment_take(&handle)?;
    mobile.0.enrollment_started()?;
    let until = Instant::now() + Duration::from_secs(60);
    let (connector, config) = workspace_runtime();
    let result = match plan.intent {
        Intent::Enroll(request) => {
            core::enroll(&connector, &config, &plan.target, &request, &plan.computer)
        }
        Intent::Check(local) => core::check(&connector, &config, &plan.target, local),
    };
    match result {
        Ok(answer) => mobile.0.enrollment_result(answer, until),
        Err(error) => {
            mobile.0.enrollment_failed();
            Err(error)
        }
    }
}
#[tauri::command(async)]
pub fn mobile_workspace_continue(
    mobile: State<'_, Mobile>,
    handle: String,
) -> Result<Reply, String> {
    let _active = mobile.0.enrollment_active()?;
    let current = {
        let mut plans = mobile
            .0
            .enrollment_plans
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if plans
            .continuation
            .as_ref()
            .is_none_or(|c| c.handle != handle)
        {
            return Err(core::CHANGED.into());
        }
        plans.continuation.take().ok_or(core::CHANGED)?
    };
    if Instant::now() >= current.until {
        return Ok(Reply {
            notice: WAITING.into(),
            connected: false,
            pending: true,
            stopped_target: None,
            continuation: None,
        });
    }
    mobile.0.enrollment_started()?;
    let (connector, config) = workspace_runtime();
    match core::check(&connector, &config, &current.target, false) {
        Ok(answer) => mobile.0.enrollment_result(answer, current.until),
        Err(error) => {
            mobile.0.enrollment_failed();
            Err(error)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Arc<Inner>, Target) {
        let inner = Arc::new(Inner::new(
            false,
            super::super::tests::service_calls_for_plans(),
            Box::new(|| {}),
        ));
        inner.page().workspace_answered(Ok(mobile::workspace(
            serde_json::json!({"state":"unconfigured","encrypted":false,"config_revision":""})
                .as_object()
                .unwrap(),
        )));
        let target = Target::capture(inner.page().view().workspace.as_ref().unwrap()).unwrap();
        (inner, target)
    }
    fn request() -> Request {
        Request {
            mode: Mode::Create,
            relay: "https://relay.example.invalid".into(),
            code: String::new(),
        }
    }
    #[test]
    fn plans_are_exact_bounded_and_consumed_before_work() {
        let (inner, target) = fixture();
        let first = inner
            .enrollment_open(&target, Intent::Enroll(request()))
            .unwrap();
        for _ in 0..8 {
            inner
                .enrollment_open(&target, Intent::Enroll(request()))
                .unwrap();
        }
        assert!(inner.enrollment_take(&first.handle).is_err());
        let current = inner
            .enrollment_open(&target, Intent::Enroll(request()))
            .unwrap();
        let (_, active) = inner.enrollment_take(&current.handle).unwrap();
        drop(active);
        assert!(inner.enrollment_take(&current.handle).is_err());
    }
    #[test]
    fn legacy_local_permission_requires_a_saved_legacy_retirement() {
        let (inner, target) = fixture();
        assert!(inner.enrollment_open(&target, Intent::Check(true)).is_err());
        let mut other = target.clone();
        other.config_revision = "b".repeat(64);
        assert!(inner
            .enrollment_open(&other, Intent::Enroll(request()))
            .is_err());
    }
    #[test]
    fn old_close_cannot_remove_another_plan() {
        let (inner, target) = fixture();
        let first = inner
            .enrollment_open(&target, Intent::Enroll(request()))
            .unwrap();
        let next = inner
            .enrollment_open(&target, Intent::Enroll(request()))
            .unwrap();
        inner.enrollment_close(&first.handle);
        assert!(inner.enrollment_take(&next.handle).is_ok());
    }
    #[test]
    fn expired_continuation_never_renews_its_deadline() {
        let (inner, _) = fixture();
        let workspace=mobile::workspace(serde_json::json!({"state":"unconfigured","encrypted":false,"config_revision":"","pending":{"state":"migration-draining","node_id":"node-new","action":"create"}}).as_object().unwrap());
        let answer = Answer {
            workspace,
            notice: "Waiting",
            continue_node: Some("node-new".into()),
            connected: false,
        };
        let reply = inner
            .enrollment_result(answer, Instant::now() - Duration::from_secs(1))
            .unwrap();
        assert!(reply.continuation.is_none());
        assert_eq!(reply.notice, WAITING);
        assert!(inner
            .enrollment_plans
            .lock()
            .unwrap()
            .continuation
            .is_none());
    }
    #[test]
    fn an_unsupported_result_clears_busy_and_exposes_a_refreshable_failure() {
        let (inner, _) = fixture();
        assert!(inner.page().read_workspace());
        let workspace=mobile::workspace(serde_json::json!({"state":"unconfigured","encrypted":false,"config_revision":"","pending":{"state":"future","node_id":"node-new","action":"create"}}).as_object().unwrap());
        assert!(inner
            .enrollment_result(
                Answer {
                    workspace,
                    notice: "Waiting",
                    continue_node: None,
                    connected: false
                },
                Instant::now()
            )
            .is_err());
        let view = inner.page().view();
        assert!(!view.workspace_busy);
        assert!(view.workspace_error.is_some());
    }
}
