//! One-use connector lifecycle intents on the existing Mobile effects lane.
use super::*;
use mobile::{
    connector_actions::{self as core, Kind},
    enrollment::Target,
};
use serde::Serialize;
use std::{collections::VecDeque, sync::atomic::AtomicBool};
struct Plan {
    target: Target,
    kind: Kind,
}
#[derive(Default)]
pub(super) struct Plans(VecDeque<(String, Plan)>);
#[derive(Serialize)]
pub struct Form {
    handle: String,
    target: Target,
    kind: Kind,
}
#[derive(Serialize)]
pub struct Reply {
    notice: &'static str,
}
struct Active<'a>(&'a AtomicBool);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
impl Inner {
    fn connector_open(&self, requested: &Target, kind: Kind) -> Result<Form, String> {
        if self.blocked {
            return Err(REPLAY.into());
        }
        let view = self.page().view();
        if view.workspace_busy || view.workspace_error.is_some() {
            return Err(core::CHANGED.into());
        }
        let target = core::admit(view.workspace.as_ref().ok_or(core::CHANGED)?, kind)?;
        if target != *requested {
            return Err(core::CHANGED.into());
        }
        let handle = uuid::Uuid::new_v4().to_string();
        let mut plans = self
            .connector_plans
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while plans.0.len() >= 8 {
            plans.0.pop_front();
        }
        plans.0.push_back((
            handle.clone(),
            Plan {
                target: target.clone(),
                kind,
            },
        ));
        Ok(Form {
            handle,
            target,
            kind,
        })
    }
    fn connector_take(&self, handle: &str) -> Result<(Plan, Active<'_>), String> {
        if self.blocked {
            return Err(REPLAY.into());
        }
        let mut plans = self
            .connector_plans
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let index = plans
            .0
            .iter()
            .position(|(id, _)| id == handle)
            .ok_or(core::CHANGED)?;
        if self.effects.swap(true, Ordering::AcqRel) {
            return Err("A mobile action is already running.".into());
        }
        let active = Active(&self.effects);
        Ok((plans.0.remove(index).ok_or(core::CHANGED)?.1, active))
    }
    fn connector_close(&self, handle: &str) {
        self.connector_plans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .0
            .retain(|(id, _)| id != handle);
    }
}
#[tauri::command(async)]
pub fn mobile_connector_open(
    mobile: State<'_, Mobile>,
    target: Target,
    kind: Kind,
) -> Result<Form, String> {
    mobile.0.connector_open(&target, kind)
}
#[tauri::command(async)]
pub fn mobile_connector_close(mobile: State<'_, Mobile>, handle: String) {
    mobile.0.connector_close(&handle);
}
#[tauri::command(async)]
pub fn mobile_connector_apply(mobile: State<'_, Mobile>, handle: String) -> Result<Reply, String> {
    let (plan, _active) = mobile.0.connector_take(&handle)?;
    if !mobile.0.page().read_workspace() {
        return Err(core::CHANGED.into());
    }
    (mobile.0.notify)();
    let (connector, config) = workspace_runtime();
    let result = core::apply(&connector, &config, &plan.target, plan.kind);
    let answer = match result {
        Ok(answer) => {
            let notice = answer.notice;
            if mobile.0.page().workspace_answered(Ok(answer.workspace)) {
                mobile.0.phones();
            }
            Ok(Reply { notice })
        }
        Err(error) => {
            mobile
                .0
                .page()
                .workspace_answered(Err(mobile::WORKSPACE_UNCONFIRMED.into()));
            Err(error)
        }
    };
    (mobile.0.notify)();
    answer
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
        let mut w=mobile::workspace(serde_json::json!({"state":"active","encrypted":true,"config_revision":"a".repeat(64),"server_url":"https://relay.example.invalid","node_id":"node-old","workspace_id":"ws-old","running":true,"connector_state":"running","generation":"e".repeat(32)}).as_object().unwrap());
        w.stop_supported = true;
        let target = core::admit(&w, Kind::Stop).unwrap();
        inner.page().workspace_answered(Ok(w));
        (inner, target)
    }
    #[test]
    fn plans_are_bounded_and_consumed_before_work() {
        let (i, t) = fixture();
        let first = i.connector_open(&t, Kind::Stop).unwrap();
        for _ in 0..8 {
            i.connector_open(&t, Kind::Stop).unwrap();
        }
        assert!(i.connector_take(&first.handle).is_err());
        let next = i.connector_open(&t, Kind::Stop).unwrap();
        let (_, active) = i.connector_take(&next.handle).unwrap();
        drop(active);
        assert!(i.connector_take(&next.handle).is_err());
    }
    #[test]
    fn admission_never_retargets_changed_metadata() {
        let (i, mut t) = fixture();
        t.generation = Some("b".repeat(32));
        assert!(i.connector_open(&t, Kind::Stop).is_err());
        assert!(i.connector_open(&t, Kind::Start).is_err());
    }
    #[test]
    fn old_close_keeps_the_new_plan() {
        let (i, t) = fixture();
        let first = i.connector_open(&t, Kind::Stop).unwrap();
        let next = i.connector_open(&t, Kind::Stop).unwrap();
        i.connector_close(&first.handle);
        assert!(i.connector_take(&next.handle).is_ok());
    }
}
