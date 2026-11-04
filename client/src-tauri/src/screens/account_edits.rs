//! Native snapshots for explicit Accounts edits. Uses the existing screens
//! catalog slot, so a background read cannot overwrite a mutation's reply.
use super::*;
use serde::Deserialize;
use std::collections::VecDeque;
use zerus_client_core::account_actions::{
    self, models::Catalog as ModelCatalog, Binding, Kind, Values,
};
pub mod distribution;
pub mod setup;

#[derive(Default)]
pub(super) struct Edits(Mutex<EditState>);
#[derive(Default)]
struct EditState {
    plans: VecDeque<(String, Plan)>,
    setups: VecDeque<(String, setup::Plan)>,
    distributions: VecDeque<(String, distribution::Plan)>,
    busy: bool,
}
struct Plan {
    kind: Kind,
    targets: Vec<Target>,
    models: Option<ModelCatalog>,
}
struct Target {
    machine: String,
    binding: Result<Binding, String>,
}
#[derive(Clone, Serialize)]
pub struct EditView {
    handle: String,
    kind: Kind,
    label: String,
    machine: String,
    provider: String,
    permission_mode: String,
    permission_detail: String,
    models: Option<ModelCatalog>,
}
#[derive(Serialize)]
pub struct EditResult {
    notices: Vec<String>,
    errors: Vec<String>,
}
#[derive(Deserialize, Default)]
pub struct EditRequest {
    handle: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    effort: String,
}

struct Busy<'a>(&'a Edits);
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0
             .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .busy = false;
    }
}
impl Screens {
    fn edit_open(
        &self,
        pump: &crate::pump::Pump,
        account: &str,
        profile: &str,
        kind: Kind,
    ) -> Result<EditView, String> {
        if !self.0.allowed {
            return Err(REPLAY_NEEDS_HGS.into());
        }
        let _opening = {
            let mut state = self
                .0
                .edits
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if state.busy {
                return Err(account_actions::UPDATING.into());
            }
            state.busy = true;
            Busy(&self.0.edits)
        };
        let view = self.accounts(false);
        let group = view
            .accounts
            .iter()
            .find(|a| a.key == account)
            .ok_or(account_actions::CHANGED)?;
        let selected = group
            .members
            .iter()
            .find(|m| m.key == profile)
            .ok_or(account_actions::CHANGED)?;
        if kind != Kind::Rename && !selected.online {
            return Err(format!("{} is offline.", selected.machine));
        }
        if kind == Kind::Default
            && (selected.is_default || matches!(selected.provider.as_str(), "dsh" | "opencode"))
        {
            return Err(account_actions::CHANGED.into());
        }
        if kind == Kind::Permissions && selected.provider == "opencode" {
            return Err("OpenCode permissions are managed in its native configuration".into());
        }
        if kind == Kind::Model && !selected.model_supported {
            return Err("This agent manages model defaults in its native settings.".into());
        }
        let members: Vec<_> = if kind == Kind::Rename {
            group.members.iter().collect()
        } else {
            vec![selected]
        };
        if members.len() > account_actions::MAX_MEMBERS {
            return Err(account_actions::CHANGED.into());
        }
        let machines: BTreeMap<_, _> = members
            .iter()
            .map(|member| {
                (
                    member.host.clone(),
                    pump.machine(&member.host).and_then(|m| m.machine_id),
                )
            })
            .collect();
        let hub = self.lock();
        let raw = hub
            .accounts
            .catalogs
            .get(&selected.host)
            .and_then(|s| s.data.as_ref())
            .and_then(|v| v["profiles"].as_array())
            .and_then(|rows| rows.iter().find(|r| r["id"] == selected.id))
            .ok_or(account_actions::CHANGED)?;
        let permission_mode = if raw["permission_mode"] == "bypass" {
            "bypass"
        } else {
            "provider"
        }
        .to_owned();
        let permission_detail = raw["permission_detail"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(2000)
            .collect();
        let targets: Vec<Target> = members
            .into_iter()
            .map(|member| {
                let binding = (|| {
                    if !member.online {
                        return Err(format!("{} is offline.", member.machine));
                    }
                    let machine = machines
                        .get(&member.host)
                        .cloned()
                        .flatten()
                        .ok_or(account_actions::CHANGED)?;
                    let catalog = hub
                        .accounts
                        .catalogs
                        .get(&member.host)
                        .and_then(|s| s.data.as_ref())
                        .ok_or(account_actions::CHANGED)?;
                    let binding =
                        Binding::capture(member.host.clone(), machine, catalog, &member.id)?;
                    if binding.provider != member.provider
                        || binding.home != member.home
                        || binding.auth_revision != member.auth_revision
                    {
                        return Err(account_actions::CHANGED.into());
                    }
                    Ok(binding)
                })();
                Target {
                    machine: member.machine.clone(),
                    binding,
                }
            })
            .collect();
        drop(hub);
        let models = if kind == Kind::Model {
            let binding = targets[0].binding.as_ref().map_err(Clone::clone)?;
            Some(binding.model_settings(&self.0.hgs)?)
        } else {
            None
        };
        let handle = uuid::Uuid::new_v4().to_string();
        let shown = EditView {
            handle: handle.clone(),
            kind,
            label: group.label.clone(),
            machine: selected.machine.clone(),
            provider: group.provider.clone(),
            permission_mode,
            permission_detail,
            models: models.clone(),
        };
        let mut edits = self
            .0
            .edits
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while edits.plans.len() >= 8 {
            edits.plans.pop_front();
        }
        edits.plans.push_back((
            handle,
            Plan {
                kind,
                targets,
                models,
            },
        ));
        Ok(shown)
    }
    fn edit_begin(&self, binding: &Binding) -> Result<(), String> {
        self.catalog_begin(&binding.computer, |catalog| binding.verify(catalog))
    }
    fn catalog_begin(
        &self,
        computer: &ComputerId,
        verify: impl FnOnce(&Value) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut hub = self.current();
        if !hub
            .fleet
            .iter()
            .any(|e| e.computer == *computer && accounts::reachable(e))
        {
            return Err("Machine is offline.".into());
        }
        let slot = hub
            .accounts
            .catalogs
            .get_mut(computer)
            .ok_or(account_actions::CHANGED)?;
        if slot.pending {
            return Err(account_actions::UPDATING.into());
        }
        verify(slot.data.as_ref().ok_or(account_actions::CHANGED)?)?;
        // Borrow the same slot as catalog reads. Explicit refresh may queue one
        // read behind this edit, but cannot race an older reply into the store.
        slot.pending = true;
        slot.requested = Some(self.now());
        hub.dirty.extend(ACCOUNT_SCREENS);
        drop(hub);
        self.publish();
        Ok(())
    }
    fn edit_finish(&self, host: &ComputerId, result: Result<Value, String>) {
        let (runs, heard) = self.catalog_reply(host.clone(), result, self.now(), true);
        self.run(runs);
        self.publish();
        self.publish_session_usage();
        if let (Some(host), Some(notify)) = (heard, self.0.launch_notify.get()) {
            notify(&host);
        }
    }

    fn edit_apply(
        &self,
        pump: &crate::pump::Pump,
        request: EditRequest,
    ) -> Result<EditResult, String> {
        let plan = {
            let mut edits = self
                .0
                .edits
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if edits.busy {
                return Err(account_actions::UPDATING.into());
            }
            let index = edits
                .plans
                .iter()
                .position(|(key, _)| key == &request.handle)
                .ok_or(account_actions::CHANGED)?;
            let (_, plan) = edits.plans.remove(index).ok_or(account_actions::CHANGED)?;
            edits.busy = true;
            plan
        };
        // The handle is consumed before any effect. No exit, retry or reopened
        // webview can dispatch this captured plan for a second time.
        let _busy = Busy(&self.0.edits);
        let values = Values {
            label: request.label,
            mode: request.mode,
            model: request.model,
            effort: request.effort,
        };
        if plan.kind == Kind::Model {
            plan.models
                .as_ref()
                .ok_or(account_actions::CHANGED)?
                .validate(&values)?;
        }
        let mut reply = EditResult {
            notices: Vec::new(),
            errors: Vec::new(),
        };
        let mut own_revisions: BTreeMap<ComputerId, (String, String)> = BTreeMap::new();
        for target in plan.targets {
            let error_machine = target
                .binding
                .as_ref()
                .ok()
                .filter(|binding| binding.computer.is_local())
                .map(|_| "This machine")
                .unwrap_or(&target.machine)
                .to_owned();
            let result = (|| {
                let mut binding = target.binding?;
                let original_revision = binding.revision.clone();
                if let Some((original, current)) = own_revisions.get(&binding.computer) {
                    if original != &original_revision {
                        return Err(account_actions::CHANGED.into());
                    }
                    // A logical account may have two profiles on one machine.
                    // Advance only through this plan's confirmed replies; all
                    // profile/auth and fresh-read checks remain unchanged.
                    binding.revision = current.clone();
                }
                if !pump.online(&binding.computer.0)
                    || pump
                        .machine(&binding.computer)
                        .and_then(|m| m.machine_id)
                        .as_deref()
                        != Some(&binding.machine_id)
                {
                    return Err(account_actions::CHANGED.into());
                }
                // Reject invalid input before borrowing the catalog slot.
                binding.edit_values(plan.kind, &values)?;
                self.edit_begin(&binding)?;
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    binding.execute_values(&self.0.hgs, plan.kind, &values)
                }))
                .unwrap_or_else(|_| Err(READ_FAILED.into()));
                self.edit_finish(&binding.computer, outcome.clone());
                outcome.map(|catalog| {
                    (
                        binding.computer,
                        original_revision,
                        catalog["revision"].as_str().unwrap_or_default().to_owned(),
                    )
                })
            })();
            match result {
                Ok((computer, original, current)) => {
                    own_revisions.insert(computer, (original, current));
                    reply.notices.push(match plan.kind {
                        Kind::Rename => continue,
                        Kind::Default => format!(
                            "Default account saved for new sessions on {}.",
                            target.machine
                        ),
                        Kind::Permissions => format!(
                            "Permission mode saved for new and resumed sessions on {}.",
                            target.machine
                        ),
                        Kind::Model => format!(
                            "Model defaults saved for new sessions on {}.",
                            target.machine
                        ),
                        Kind::Remove => format!(
                            "Account removed from {}. Add it again whenever you need it.",
                            target.machine
                        ),
                    });
                }
                Err(error) => {
                    reply.errors.push(format!("{error_machine}: {error}"));
                    // AccountsPage clears its remaining rename queue on the
                    // first failed member; a partial rename is never replayed.
                    if plan.kind == Kind::Rename {
                        break;
                    }
                }
            }
        }
        if plan.kind == Kind::Rename && reply.errors.is_empty() {
            reply.notices.push("Account name updated.".into());
        }
        reply.notices.dedup();
        Ok(reply)
    }
}

#[cfg(feature = "live")]
#[tauri::command(async)]
pub fn account_edit_open(
    state: State<'_, Screens>,
    source: State<'_, crate::commands::AppState>,
    account: String,
    profile: String,
    kind: Kind,
) -> Result<EditView, String> {
    state.edit_open(&source.pump(), &account, &profile, kind)
}
#[cfg(feature = "live")]
#[tauri::command(async)]
pub fn account_edit_apply(
    state: State<'_, Screens>,
    source: State<'_, crate::commands::AppState>,
    request: EditRequest,
) -> Result<EditResult, String> {
    state.edit_apply(&source.pump(), request)
}
#[tauri::command]
pub fn account_edit_close(state: State<'_, Screens>, handle: String) {
    state
        .0
        .edits
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .plans
        .retain(|(key, _)| key != &handle);
}
#[cfg(not(feature = "live"))]
#[tauri::command]
pub fn account_edit_open() -> Result<EditView, String> {
    Err(REPLAY_NEEDS_HGS.into())
}
#[cfg(not(feature = "live"))]
#[tauri::command]
pub fn account_edit_apply() -> Result<EditResult, String> {
    Err(REPLAY_NEEDS_HGS.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zerus_client_core::{Change, ComputerStatus, MachineMeta};
    pub(super) fn fixture() -> (Screens, crate::pump::Pump, Binding) {
        fixture_at(PathBuf::from("unused-hgs"))
    }
    pub(super) fn fixture_at(hgs: PathBuf) -> (Screens, crate::pump::Pump, Binding) {
        let local = ComputerId::local();
        let fleet = vec![FleetEntry {
            computer: local.clone(),
            status: "live".into(),
            attempt: 0,
            local_sessions: true,
            local_host: false,
            host: Some("workstation".into()),
            platform: Some("linux".into()),
        }];
        let screens = Screens::new(
            hgs,
            true,
            Arc::new(|_| {}),
            Arc::new(|_| {}),
            Arc::new(move || fleet.clone()),
            Arc::new(|| 0),
        );
        let catalog = json!({"revision":"r1","setup_revision":true,"profiles":[{"id":"work","provider":"codex","label":"Example","home":"/accounts/example","auth_revision":"a1","installed":false}]});
        screens.lock().accounts.catalogs.insert(
            local.clone(),
            accounts::CatalogSlot {
                requested: Some(0),
                data: Some(catalog.clone()),
                ..Default::default()
            },
        );
        let pump = crate::pump::Pump::default();
        pump.push(Change::Computer {
            computer: local.clone(),
            status: ComputerStatus::Live,
        });
        let machine = "11111111-1111-4111-8111-111111111111";
        pump.push(Change::Machine {
            computer: local.clone(),
            machine: MachineMeta {
                machine_id: Some(machine.into()),
                ..Default::default()
            },
        });
        let binding = Binding::capture(local, machine.into(), &catalog, "work").unwrap();
        (screens, pump, binding)
    }
    #[test]
    fn captures_the_native_group_and_consumes_a_stale_handle_once() {
        let (screens, pump, _) = fixture();
        let view = screens.accounts(false);
        let group = &view.accounts[0];
        let opened = screens
            .edit_open(&pump, &group.key, &group.members[0].key, Kind::Rename)
            .unwrap();
        screens
            .lock()
            .accounts
            .catalogs
            .get_mut(&ComputerId::local())
            .unwrap()
            .data
            .as_mut()
            .unwrap()["revision"] = json!("r2");
        let result = screens
            .edit_apply(
                &pump,
                EditRequest {
                    handle: opened.handle.clone(),
                    label: "Updated".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].contains(account_actions::CHANGED));
        assert!(screens
            .edit_apply(
                &pump,
                EditRequest {
                    handle: opened.handle,
                    label: "Updated".into(),
                    ..Default::default()
                }
            )
            .is_err());
        assert!(!screens.0.edits.0.lock().unwrap().busy);
    }
    #[test]
    fn a_changed_physical_machine_refuses_the_captured_profile() {
        let (screens, pump, _) = fixture();
        let view = screens.accounts(false);
        let group = &view.accounts[0];
        let opened = screens
            .edit_open(&pump, &group.key, &group.members[0].key, Kind::Remove)
            .unwrap();
        pump.push(Change::Machine {
            computer: ComputerId::local(),
            machine: MachineMeta {
                machine_id: Some("22222222-2222-4222-8222-222222222222".into()),
                ..Default::default()
            },
        });
        let result = screens
            .edit_apply(
                &pump,
                EditRequest {
                    handle: opened.handle,
                    label: String::new(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(result.notices.is_empty());
        assert!(result.errors[0].contains(account_actions::CHANGED));
    }
    #[test]
    fn uses_the_existing_catalog_slot_and_releases_it_after_a_failed_effect() {
        let (screens, _, binding) = fixture();
        screens.edit_begin(&binding).unwrap();
        assert_eq!(
            screens.edit_begin(&binding).unwrap_err(),
            account_actions::UPDATING
        );
        screens.edit_finish(&binding.computer, Err("Uncertain result".into()));
        let hub = screens.lock();
        let slot = &hub.accounts.catalogs[&binding.computer];
        assert!(!slot.pending);
        assert_eq!(slot.data.as_ref().unwrap()["revision"], "r1");
        assert_eq!(
            slot.error, None,
            "mutation errors use the notice bus, not a catalog-read banner"
        );
    }
    #[test]
    fn native_pending_handles_have_a_fixed_bound() {
        let (screens, pump, _) = fixture();
        let view = screens.accounts(false);
        let group = &view.accounts[0];
        for _ in 0..20 {
            screens
                .edit_open(&pump, &group.key, &group.members[0].key, Kind::Remove)
                .unwrap();
        }
        assert_eq!(screens.0.edits.0.lock().unwrap().plans.len(), 8);
    }
    #[test]
    fn a_model_open_shares_single_admission_and_cannot_clear_another_operation() {
        let (screens, pump, _) = fixture();
        let view = screens.accounts(false);
        let account = &view.accounts[0];
        screens.0.edits.0.lock().unwrap().busy = true;
        assert!(screens
            .edit_open(&pump, &account.key, &account.members[0].key, Kind::Model)
            .is_err());
        assert!(screens.0.edits.0.lock().unwrap().busy);
    }
    #[test]
    fn permissions_capture_the_raw_override_and_models_reject_forged_choices() {
        let (screens, pump, binding) = fixture();
        screens
            .lock()
            .accounts
            .catalogs
            .get_mut(&binding.computer)
            .unwrap()
            .data
            .as_mut()
            .unwrap()["profiles"][0]["permission_mode"] = json!("bypass");
        let view = screens.accounts(false);
        let account = &view.accounts[0];
        let opened = screens
            .edit_open(
                &pump,
                &account.key,
                &account.members[0].key,
                Kind::Permissions,
            )
            .unwrap();
        assert_eq!(opened.permission_mode, "bypass");
        assert!(opened.models.is_none());
        screens.0.edits.0.lock().unwrap().plans.push_back((
            "model".into(),
            Plan {
                kind: Kind::Model,
                targets: vec![Target {
                    machine: "workstation".into(),
                    binding: Ok(binding),
                }],
                models: Some(ModelCatalog {
                    models: Vec::new(),
                    defaults: Default::default(),
                    provider_defaults: Default::default(),
                    effort_supported: true,
                }),
            },
        ));
        let request = EditRequest {
            handle: "model".into(),
            model: "forged".into(),
            ..Default::default()
        };
        assert!(screens.edit_apply(&pump, request).is_err());
        assert!(!screens.0.edits.0.lock().unwrap().busy);
        assert!(!screens
            .0
            .edits
            .0
            .lock()
            .unwrap()
            .plans
            .iter()
            .any(|(id, _)| id == "model"));
    }
    #[test]
    fn a_new_session_opened_during_a_write_receives_its_catalog_reply() {
        let (screens, _, binding) = fixture();
        screens.edit_begin(&binding).unwrap();
        assert!(screens
            .launch_start(&binding.computer, false, Some("codex"))
            .is_empty());
        assert!(!screens.launch_rows(&binding.computer).loaded);
        let catalog = screens.lock().accounts.catalogs[&binding.computer]
            .data
            .clone()
            .unwrap();
        screens.edit_finish(&binding.computer, Ok(catalog));
        assert!(screens.launch_rows(&binding.computer).loaded);
    }
    #[test]
    fn rename_stops_at_the_first_failed_member_as_qt_does() {
        let (screens, pump, _) = fixture();
        screens.0.edits.0.lock().unwrap().plans.push_back((
            "captured".into(),
            Plan {
                models: None,
                kind: Kind::Rename,
                targets: vec![
                    Target {
                        machine: "studio".into(),
                        binding: Err("offline".into()),
                    },
                    Target {
                        machine: "other".into(),
                        binding: Err("must not run".into()),
                    },
                ],
            },
        ));
        let result = screens
            .edit_apply(
                &pump,
                EditRequest {
                    handle: "captured".into(),
                    label: "Updated".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(result.notices.is_empty());
        assert_eq!(result.errors, ["studio: offline"]);
    }
    #[cfg(unix)]
    #[test]
    fn rename_advances_only_its_own_revision_for_two_profiles_on_one_machine() {
        use std::os::unix::fs::PermissionsExt;
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "zerus-account-rename-{}-{stamp}",
            std::process::id()
        )));
        std::fs::create_dir(&scratch.0).unwrap();
        let hgs = scratch.0.join("hgs");
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../core/tests/support/fake_hgs_account_edits.py"),
            &hgs,
        )
        .unwrap();
        std::fs::set_permissions(&hgs, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (screens, pump, first) = fixture_at(hgs);
        let mut catalog = screens.lock().accounts.catalogs[&first.computer]
            .data
            .clone()
            .unwrap();
        let mut second = catalog["profiles"][0].clone();
        second["id"] = json!("second");
        second["home"] = json!("/accounts/second");
        catalog["profiles"].as_array_mut().unwrap().push(second);
        std::fs::write(scratch.0.join("catalog.json"), catalog.to_string()).unwrap();
        screens
            .lock()
            .accounts
            .catalogs
            .get_mut(&first.computer)
            .unwrap()
            .data = Some(catalog.clone());
        let second = Binding::capture(
            first.computer.clone(),
            first.machine_id.clone(),
            &catalog,
            "second",
        )
        .unwrap();
        screens.0.edits.0.lock().unwrap().plans.push_back((
            "same-machine".into(),
            Plan {
                models: None,
                kind: Kind::Rename,
                targets: vec![
                    Target {
                        machine: "workstation".into(),
                        binding: Ok(first),
                    },
                    Target {
                        machine: "workstation".into(),
                        binding: Ok(second),
                    },
                ],
            },
        ));
        let result = screens
            .edit_apply(
                &pump,
                EditRequest {
                    handle: "same-machine".into(),
                    label: "Updated".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.notices, ["Account name updated."]);
        let saved: Value =
            serde_json::from_slice(&std::fs::read(scratch.0.join("catalog.json")).unwrap())
                .unwrap();
        assert_eq!(saved["revision"], "r3");
        assert!(saved["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["label"] == "Updated"));
        assert_eq!(
            std::fs::read_to_string(scratch.0.join("calls.jsonl"))
                .unwrap()
                .lines()
                .count(),
            4
        );
    }
}
