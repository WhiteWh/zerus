//! One-use computer invitation admission and one transient metadata handle.
use super::*;
use mobile::computer_codes::{self as codes, Invitation, Target};
use serde::Serialize;
use std::{
    collections::VecDeque,
    sync::atomic::AtomicBool,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
struct Issued {
    handle: String,
    target: Target,
    invitation_id: String,
    expires_at: f64,
}
#[derive(Default)]
pub(super) struct Plans {
    pending: VecDeque<(String, Target)>,
    issued: Option<Issued>,
}
#[derive(Serialize)]
pub struct Code {
    handle: String,
    target: Target,
    #[serde(flatten)]
    invitation: Invitation,
}
struct Active<'a>(&'a AtomicBool);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
impl Inner {
    fn code_active(&self) -> Result<Active<'_>, String> {
        if self.blocked {
            return Err(REPLAY.into());
        }
        if self.effects.swap(true, Ordering::AcqRel) {
            return Err("A mobile action is already running.".into());
        }
        Ok(Active(&self.effects))
    }
    fn computer_open(&self, requested: &Target) -> Result<String, String> {
        if self.blocked {
            return Err(REPLAY.into());
        }
        let view = self.page().view();
        if view.workspace_busy || view.workspace_error.is_some() {
            return Err(codes::CHANGED.into());
        }
        let target = Target::capture(view.workspace.as_ref().ok_or(codes::CHANGED)?)?;
        if target != *requested {
            return Err(codes::CHANGED.into());
        }
        let mut plans = self
            .computer_plans
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if plans
            .issued
            .as_ref()
            .is_some_and(|v| v.target == target && v.expires_at > now())
        {
            return Err("A computer code is already active.".into());
        }
        plans.issued = None;
        while plans.pending.len() >= 8 {
            plans.pending.pop_front();
        }
        let handle = uuid::Uuid::new_v4().to_string();
        plans.pending.push_back((handle.clone(), target));
        Ok(handle)
    }
    fn take_computer(&self, handle: &str) -> Result<(Target, Active<'_>), String> {
        let mut plans = self
            .computer_plans
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let at = plans
            .pending
            .iter()
            .position(|(h, _)| h == handle)
            .ok_or(codes::CHANGED)?;
        let active = self.code_active()?;
        let target = plans.pending.remove(at).ok_or(codes::CHANGED)?.1;
        Ok((target, active))
    }
    fn issued_computer(&self, handle: &str, consume: bool) -> Result<(Issued, Active<'_>), String> {
        let mut plans = self
            .computer_plans
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let issued = plans
            .issued
            .as_ref()
            .filter(|i| i.handle == handle)
            .cloned()
            .ok_or(codes::CHANGED)?;
        let active = self.code_active()?;
        if consume {
            plans.issued = None;
        }
        Ok((issued, active))
    }
    fn close_computer(&self, handle: &str) {
        let mut plans = self
            .computer_plans
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        plans.pending.retain(|(h, _)| h != handle);
        if plans.issued.as_ref().is_some_and(|i| i.handle == handle) {
            plans.issued = None;
        }
    }
}
#[tauri::command(async)]
pub fn mobile_computer_open(mobile: State<'_, Mobile>, target: Target) -> Result<String, String> {
    mobile.0.computer_open(&target)
}
#[tauri::command(async)]
pub fn mobile_computer_close(mobile: State<'_, Mobile>, handle: String) {
    mobile.0.close_computer(&handle);
}
#[tauri::command(async)]
pub fn mobile_computer_invite(mobile: State<'_, Mobile>, handle: String) -> Result<Code, String> {
    let (target, _active) = mobile.0.take_computer(&handle)?;
    let (connector, config) = workspace_runtime();
    let invitation = codes::invite(&connector, &config, &target)?;
    let handle = uuid::Uuid::new_v4().to_string();
    mobile
        .0
        .computer_plans
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .issued = Some(Issued {
        handle: handle.clone(),
        target: target.clone(),
        invitation_id: invitation.invitation_id.clone(),
        expires_at: invitation.expires_at,
    });
    Ok(Code {
        handle,
        target,
        invitation,
    })
}
#[tauri::command(async)]
pub fn mobile_computer_cancel(mobile: State<'_, Mobile>, handle: String) -> Result<(), String> {
    // Consume before dispatch, including every uncertain outcome.
    let (issued, _active) = mobile.0.issued_computer(&handle, true)?;
    let (connector, config) = workspace_runtime();
    codes::cancel(&connector, &config, &issued.target, &issued.invitation_id)
}
#[tauri::command(async)]
pub fn mobile_computer_check(mobile: State<'_, Mobile>, handle: String) -> Result<(), String> {
    let (issued, _active) = mobile.0.issued_computer(&handle, false)?;
    if issued.expires_at <= now() {
        return Err(codes::CHANGED.into());
    }
    if !mobile.0.page().read_workspace() {
        return Ok(());
    }
    (mobile.0.notify)();
    let (connector, config) = workspace_runtime();
    let result = codes::check(&connector, &config, &issued.target);
    let error = result.as_ref().err().cloned();
    if mobile.0.page().workspace_answered(result) {
        mobile.0.phones();
    }
    (mobile.0.notify)();
    error.map_or(Ok(()), Err)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Inner, Target) {
        let target = Target {
            config_revision: "a".repeat(64),
            server_url: "https://relay.example.invalid".into(),
        };
        let inner = Inner::new(
            false,
            super::super::tests::service_calls_for_plans(),
            Box::new(|| {}),
        );
        inner.page().workspace_answered(Ok(mobile::workspace(serde_json::json!({"state":"active","encrypted":true,"config_revision":target.config_revision,"server_url":target.server_url}).as_object().unwrap())));
        (inner, target)
    }
    #[test]
    fn admission_is_snapshot_bound_bounded_and_consumed_once() {
        let (inner, target) = fixture();
        let first = inner.computer_open(&target).unwrap();
        for _ in 0..8 {
            inner.computer_open(&target).unwrap();
        }
        assert_eq!(inner.computer_plans.lock().unwrap().pending.len(), 8);
        assert!(inner.take_computer(&first).is_err());
        let handle = inner.computer_open(&target).unwrap();
        let (_, active) = inner.take_computer(&handle).unwrap();
        drop(active);
        assert!(inner.take_computer(&handle).is_err());
        assert!(inner
            .computer_open(&Target {
                config_revision: "b".repeat(64),
                ..target
            })
            .is_err());
    }
    #[test]
    fn the_effect_gate_is_shared_with_phone_actions_and_replay_refuses() {
        let (mut inner, target) = fixture();
        let handle = inner.computer_open(&target).unwrap();
        inner.effects.store(true, Ordering::Release);
        assert!(inner.take_computer(&handle).is_err());
        inner.effects.store(false, Ordering::Release);
        let (_, active) = inner.take_computer(&handle).unwrap();
        assert!(inner.effects.load(Ordering::Acquire));
        drop(active);
        inner.blocked = true;
        assert!(inner.computer_open(&target).is_err());
    }
    #[test]
    fn cancel_consumes_the_issued_identity_and_old_close_cannot_clear_another_code() {
        let (inner, target) = fixture();
        inner.computer_plans.lock().unwrap().issued = Some(Issued {
            handle: "issued".into(),
            target: target.clone(),
            invitation_id: "original-id".into(),
            expires_at: now() + 600.,
        });
        inner.close_computer("old");
        assert!(inner.computer_open(&target).is_err());
        let (value, active) = inner.issued_computer("issued", true).unwrap();
        assert_eq!(value.invitation_id, "original-id");
        drop(active);
        assert!(inner.issued_computer("issued", true).is_err());
        assert!(inner.computer_open(&target).is_ok());
    }
}
