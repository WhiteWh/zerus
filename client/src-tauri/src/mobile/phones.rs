//! Bounded one-use phone plans; no credential enters the shared page/event store.
use super::*;
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, sync::atomic::AtomicBool};
use zerus_client_core::mobile::phone_actions::{self, Invitation, Target};
#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Invite,
    Revoke,
}
struct Plan {
    kind: Kind,
    target: Target,
    phone: Option<String>,
}
#[derive(Default)]
pub(super) struct Plans(VecDeque<(String, Plan)>);
#[derive(Serialize)]
pub struct Form {
    handle: String,
    pub target: Target,
    pub phone: Option<String>,
    pub name: Option<String>,
}
#[derive(Serialize)]
pub struct Code {
    #[serde(flatten)]
    invite: Invitation,
    qr: Vec<bool>,
    size: i32,
}
struct Active<'a>(&'a AtomicBool);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
impl Inner {
    fn phone_open(&self, kind: Kind, target: &Target, phone: Option<&str>) -> Result<Form, String> {
        if self.blocked {
            return Err(REPLAY.into());
        }
        let view = self.page().view();
        if view.phones_busy {
            return Err("Checking phones…".into());
        }
        let list = view
            .phones
            .as_ref()
            .and_then(|v| {
                v.lists.iter().find(|v| {
                    v.host == target.host
                        && v.workspace_id == target.workspace_id
                        && v.server_url.trim_end_matches('/')
                            == target.server_url.trim_end_matches('/')
                })
            })
            .ok_or(phone_actions::CHANGED)?;
        let captured = Target::capture(list)?;
        let name = match kind {
            Kind::Invite => None,
            Kind::Revoke => Some(
                list.phones
                    .iter()
                    .find(|p| Some(p.id.as_str()) == phone)
                    .ok_or(phone_actions::CHANGED)?
                    .name
                    .clone(),
            ),
        };
        let handle = uuid::Uuid::new_v4().to_string();
        let mut plans = self
            .phone_plans
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while plans.0.len() >= 8 {
            plans.0.pop_front();
        }
        plans.0.push_back((
            handle.clone(),
            Plan {
                kind,
                target: captured.clone(),
                phone: phone.map(str::to_owned),
            },
        ));
        Ok(Form {
            handle,
            target: captured,
            phone: phone.map(str::to_owned),
            name,
        })
    }
    fn take_phone(&self, handle: &str, kind: Kind) -> Result<(Plan, Active<'_>), String> {
        if self.blocked {
            return Err(REPLAY.into());
        }
        let mut plans = self
            .phone_plans
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let at = plans
            .0
            .iter()
            .position(|(h, p)| h == handle && p.kind == kind)
            .ok_or(phone_actions::CHANGED)?;
        if self.effects.swap(true, Ordering::AcqRel) {
            return Err("A mobile action is already running.".into());
        }
        let plan = plans.0.remove(at).ok_or(phone_actions::CHANGED)?.1;
        Ok((plan, Active(&self.effects)))
    }
}
#[tauri::command(async)]
pub fn mobile_phone_open(
    mobile: State<'_, Mobile>,
    kind: Kind,
    target: Target,
    phone: Option<String>,
) -> Result<Form, String> {
    mobile.0.phone_open(kind, &target, phone.as_deref())
}
#[tauri::command(async)]
pub fn mobile_phone_close(mobile: State<'_, Mobile>, handle: String) {
    mobile
        .0
        .phone_plans
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .0
        .retain(|(h, _)| h != &handle);
}
#[tauri::command(async)]
pub fn mobile_phone_invite(mobile: State<'_, Mobile>, handle: String) -> Result<Code, String> {
    let (plan, _active) = mobile.0.take_phone(&handle, Kind::Invite)?;
    let invite = phone_actions::invite(&default_hgs(), &plan.target)?;
    let encoded =
        qrcodegen::QrCode::encode_text(&invite.pair_url, qrcodegen::QrCodeEcc::Medium).ok();
    let size = encoded.as_ref().map(|q| q.size()).unwrap_or(0);
    let qr = encoded
        .map(|q| {
            (0..size)
                .flat_map(|y| (0..size).map(move |x| (x, y)))
                .map(|(x, y)| q.get_module(x, y))
                .collect()
        })
        .unwrap_or_default();
    Ok(Code { invite, qr, size })
}
#[tauri::command(async)]
pub fn mobile_phone_revoke(mobile: State<'_, Mobile>, handle: String) -> Result<(), String> {
    let (plan, _active) = mobile.0.take_phone(&handle, Kind::Revoke)?;
    let result = phone_actions::revoke(
        &default_hgs(),
        &plan.target,
        plan.phone.as_deref().ok_or(phone_actions::CHANGED)?,
    );
    if result.is_ok() {
        mobile.0.phones();
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Inner, Target) {
        let target = Target {
            host: String::new(),
            workspace_id: "workspace-a".into(),
            workspace_name: "Example".into(),
            server_url: "https://relay.example.invalid".into(),
            encrypted: true,
        };
        let inner = Inner::new(
            false,
            super::super::tests::service_calls_for_plans(),
            Box::new(|| {}),
        );
        let list = mobile::PhoneList {
            host: target.host.clone(),
            workspace_id: target.workspace_id.clone(),
            workspace_name: target.workspace_name.clone(),
            server_url: target.server_url.clone(),
            encrypted: true,
            invite: String::new(),
            phones: vec![mobile::Phone {
                id: "11111111-1111-4111-8111-111111111111".into(),
                name: "Example phone".into(),
                platform: "iOS".into(),
                paired_at: None,
                last_seen_at: None,
                added_from: None,
                revoke: String::new(),
            }],
        };
        inner.page().phones_answered(mobile::Phones {
            lists: vec![list],
            lines: vec![],
        });
        (inner, target)
    }
    #[test]
    fn plans_pin_the_listed_workspace_and_are_consumed_once() {
        let (inner, target) = fixture();
        let f = inner
            .phone_open(
                Kind::Revoke,
                &target,
                Some("11111111-1111-4111-8111-111111111111"),
            )
            .unwrap();
        assert!(inner.take_phone(&f.handle, Kind::Invite).is_err());
        let (plan, active) = inner.take_phone(&f.handle, Kind::Revoke).unwrap();
        assert_eq!(plan.target, target);
        drop(active);
        assert!(inner.take_phone(&f.handle, Kind::Revoke).is_err());
    }
    #[test]
    fn changed_unlisted_target_cannot_obtain_a_plan() {
        let (inner, mut target) = fixture();
        target.workspace_id = "other".into();
        assert!(inner.phone_open(Kind::Invite, &target, None).is_err());
    }
    #[test]
    fn replay_is_blocked_and_plans_are_bounded() {
        let (mut inner, target) = fixture();
        inner.blocked = true;
        assert!(inner.phone_open(Kind::Invite, &target, None).is_err());
        inner.blocked = false;
        let first = inner.phone_open(Kind::Invite, &target, None).unwrap();
        for _ in 0..8 {
            inner.phone_open(Kind::Invite, &target, None).unwrap();
        }
        assert_eq!(inner.phone_plans.lock().unwrap().0.len(), 8);
        assert!(inner.take_phone(&first.handle, Kind::Invite).is_err());
        assert!(!inner.effects.load(Ordering::Acquire));
    }
    #[test]
    fn the_real_encoder_has_standard_finder_modules() {
        let qr =
            qrcodegen::QrCode::encode_text("HELLO WORLD", qrcodegen::QrCodeEcc::Medium).unwrap();
        assert_eq!(qr.size(), 21);
        assert!(qr.get_module(0, 0));
        assert!(!qr.get_module(7, 0));
        assert!(qr.get_module(3, 3));
    }
}
