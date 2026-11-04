//! Explicit account setup on the existing catalog writer and terminal backend.
use super::*;
use zerus_client_core::account_actions::setup::{self as policy, Host as HostBinding};
use zerus_client_core::terminal_launch::Launcher;

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SetupKind {
    Add,
    Login,
    Install,
    Start,
    Key,
}
pub(super) struct Plan {
    kind: SetupKind,
    hosts: Vec<HostPlan>,
    selected: Option<Binding>,
    label: String,
}
struct HostPlan {
    binding: HostBinding,
    saved: Vec<Value>,
    providers: Vec<String>,
    machine: String,
}
#[derive(Clone, Serialize)]
pub struct Saved {
    id: String,
    provider: String,
    label: String,
    installed: bool,
}
#[derive(Clone, Serialize)]
pub struct SetupHost {
    key: String,
    machine: String,
    label: String,
    online: bool,
    providers: Vec<String>,
    saved: Vec<Saved>,
    listed: Vec<String>,
    deepseek_installed: bool,
}
#[derive(Serialize)]
pub struct SetupView {
    handle: String,
    kind: SetupKind,
    hosts: Vec<SetupHost>,
    host: String,
    provider: String,
    label: String,
}
#[derive(Deserialize)]
pub struct SetupRequest {
    handle: String,
    #[serde(default)]
    host: String,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    saved: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    api_key: String,
}
#[derive(Serialize)]
pub struct SetupResult {
    notices: Vec<String>,
    errors: Vec<String>,
    profile: Option<String>,
}
pub(super) fn row_evidence(row: &Value) -> Value {
    serde_json::json!({"id":row["id"],"provider":row["provider"],"home":row["home"],"auth_revision":row["auth_revision"]})
}
fn same_machine(pump: &crate::pump::Pump, host: &HostBinding) -> bool {
    pump.online(&host.computer.0)
        && pump
            .machine(&host.computer)
            .and_then(|m| m.machine_id)
            .as_deref()
            == Some(&host.machine_id)
}
pub(super) fn setup_terminal(
    dir: &std::path::Path,
    hgs: &std::path::Path,
    args: &[String],
) -> Result<Option<String>, String> {
    let mut terminal = Launcher::new(crate::desk::terminal_settings(dir));
    terminal.open_program(&hgs.to_string_lossy(), args)
}
impl Screens {
    fn setup_open(
        &self,
        pump: &crate::pump::Pump,
        kind: SetupKind,
        account: &str,
        profile: &str,
    ) -> Result<SetupView, String> {
        if !self.0.allowed {
            return Err(REPLAY_NEEDS_HGS.into());
        }
        let _busy = {
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
        let selected = if kind == SetupKind::Add {
            None
        } else {
            Some(
                view.accounts
                    .iter()
                    .find(|a| a.key == account)
                    .and_then(|a| a.members.iter().find(|m| m.key == profile))
                    .ok_or(account_actions::CHANGED)?,
            )
        };
        let machines: BTreeMap<_, _> = view
            .hosts
            .iter()
            .map(|h| {
                (
                    h.computer.clone(),
                    pump.machine(&h.computer).and_then(|m| m.machine_id),
                )
            })
            .collect();
        let hub = self.current();
        let mut hosts = Vec::new();
        let mut shown = Vec::new();
        for host in view
            .hosts
            .iter()
            .filter(|h| selected.is_none_or(|s| s.host == h.computer))
            .take(64)
        {
            let Some(catalog) = hub
                .accounts
                .catalogs
                .get(&host.computer)
                .and_then(|s| s.data.as_ref())
            else {
                continue;
            };
            let Some(machine) = machines.get(&host.computer).cloned().flatten() else {
                continue;
            };
            let binding = HostBinding::capture(host.computer.clone(), machine, catalog)?;
            let entry = hub
                .fleet
                .iter()
                .find(|e| e.computer == host.computer)
                .ok_or(account_actions::CHANGED)?;
            let providers: Vec<String> = ["codex", "claude", "kimi", "dsh", "opencode"]
                .into_iter()
                .filter(|p| accounts::launchable(entry, p))
                .map(str::to_owned)
                .collect();
            let saved: Vec<_> = catalog["removed_profiles"]
                .as_array()
                .into_iter()
                .flatten()
                .take(256)
                .collect();
            shown.push(SetupHost {
                key: host.computer.0.clone(),
                machine: host.name.clone(),
                label: host.label.clone(),
                online: host.online,
                providers: providers.clone(),
                saved: saved
                    .iter()
                    .map(|row| {
                        let label = row["label"].as_str().unwrap_or("");
                        let email = row["account_status"]["identity"]["email"]
                            .as_str()
                            .unwrap_or("");
                        Saved {
                            id: row["id"].as_str().unwrap_or("").into(),
                            provider: row["provider"].as_str().unwrap_or("").into(),
                            label: if email.is_empty() || email == label {
                                label.into()
                            } else {
                                format!("{label} ({email})")
                            },
                            installed: row["installed"] == true,
                        }
                    })
                    .collect(),
                listed: catalog["profiles"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|r| r["id"].as_str().map(str::to_owned))
                    .take(256)
                    .collect(),
                deepseek_installed: ["profiles", "removed_profiles"].iter().any(|f| {
                    catalog[*f].as_array().is_some_and(|rows| {
                        rows.iter()
                            .any(|r| r["id"] == "native-dsh" && r["installed"] == true)
                    })
                }),
            });
            hosts.push(HostPlan {
                binding,
                saved: saved.iter().map(|r| row_evidence(r)).collect(),
                providers,
                machine: host.name.clone(),
            });
        }
        let captured = if let Some(m) = selected {
            if !m.online {
                return Err(format!("{} is offline.", m.machine));
            }
            let host = hosts.first().ok_or(account_actions::CHANGED)?;
            let catalog = hub
                .accounts
                .catalogs
                .get(&m.host)
                .and_then(|s| s.data.as_ref())
                .ok_or(account_actions::CHANGED)?;
            let b = Binding::capture(
                m.host.clone(),
                host.binding.machine_id.clone(),
                catalog,
                &m.id,
            )?;
            if b.provider != m.provider || b.home != m.home || b.auth_revision != m.auth_revision {
                return Err(account_actions::CHANGED.into());
            }
            match kind {
                SetupKind::Login
                    if !m.installed
                        || !["codex", "claude", "kimi"].contains(&m.provider.as_str()) =>
                {
                    return Err("Sign-in is unavailable for this account.".into())
                }
                SetupKind::Install if m.installed || m.provider == "opencode" => {
                    return Err("An installer is unavailable for this account.".into())
                }
                SetupKind::Start
                    if m.provider != "opencode"
                        || !m.opencode.as_ref().is_some_and(|o| o.stopped) =>
                {
                    return Err(account_actions::CHANGED.into())
                }
                SetupKind::Key if m.provider != "dsh" || !m.installed => {
                    return Err(
                        "Install DeepSeek on this machine before saving its API key.".into(),
                    )
                }
                _ => {}
            }
            Some(b)
        } else {
            None
        };
        drop(hub);
        let handle = uuid::Uuid::new_v4().to_string();
        let label = view
            .accounts
            .iter()
            .find(|a| a.key == account)
            .map(|a| a.label.clone())
            .unwrap_or_default();
        let result = SetupView {
            handle: handle.clone(),
            kind,
            host: selected.map(|m| m.host.0.clone()).unwrap_or_default(),
            provider: selected
                .map(|m| m.provider.clone())
                .unwrap_or_else(|| "codex".into()),
            label: label.clone(),
            hosts: shown,
        };
        let mut state = self
            .0
            .edits
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while state.setups.len() >= 8 {
            state.setups.pop_front();
        }
        state.setups.push_back((
            handle,
            Plan {
                kind,
                hosts,
                selected: captured,
                label,
            },
        ));
        Ok(result)
    }
    fn setup_apply(
        &self,
        pump: &crate::pump::Pump,
        dir: &std::path::Path,
        request: SetupRequest,
    ) -> Result<SetupResult, String> {
        let plan = {
            let mut state = self
                .0
                .edits
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if state.busy {
                return Err(account_actions::UPDATING.into());
            }
            let i = state
                .setups
                .iter()
                .position(|(h, _)| h == &request.handle)
                .ok_or(account_actions::CHANGED)?;
            let (_, plan) = state.setups.remove(i).ok_or(account_actions::CHANGED)?;
            state.busy = true;
            plan
        };
        let _busy = Busy(&self.0.edits);
        let host = plan
            .hosts
            .iter()
            .find(|h| {
                h.binding.computer.0
                    == plan
                        .selected
                        .as_ref()
                        .map(|b| b.computer.0.as_str())
                        .unwrap_or(&request.host)
            })
            .ok_or(account_actions::CHANGED)?;
        if !same_machine(pump, &host.binding) {
            return Err(account_actions::CHANGED.into());
        }
        let provider = plan
            .selected
            .as_ref()
            .map(|b| b.provider.as_str())
            .unwrap_or(&request.provider);
        if !host.providers.iter().any(|p| p == provider) {
            return Err(account_actions::CHANGED.into());
        }
        if matches!(
            plan.kind,
            SetupKind::Login | SetupKind::Install | SetupKind::Start
        ) || (plan.kind == SetupKind::Add && request.saved.is_empty() && provider != "dsh")
        {
            host.binding.terminal_call("login", "")?;
        }
        self.catalog_begin(&host.binding.computer, |v| {
            host.binding.verify(v)?;
            if let Some(b) = &plan.selected {
                b.verify(v)?;
            }
            Ok(())
        })?;
        let execute = || {
            let mut profile = None;
            let mut notices = Vec::new();
            let mut errors = Vec::new();
            let mut catalog = host.binding.fresh(&self.0.hgs)?;
            if let Some(b) = &plan.selected {
                b.verify(&catalog)?;
            }
            match plan.kind {
                SetupKind::Add if !request.saved.is_empty() => {
                    let saved = host
                        .saved
                        .iter()
                        .find(|r| r["id"] == request.saved && r["provider"] == provider)
                        .ok_or(account_actions::CHANGED)?;
                    catalog = host.binding.restore(&self.0.hgs, saved)?;
                    profile = Some(format!(
                        "{}\u{1f}{}",
                        host.binding.computer.0, request.saved
                    ));
                    notices.push(format!(
                        "Account added on {} using its saved sign-in.",
                        host.machine
                    ));
                }
                SetupKind::Key => {
                    catalog = host.binding.set_key(
                        &self.0.hgs,
                        Some(&plan.label),
                        request.api_key.trim(),
                    )?;
                    notices.push(format!("DeepSeek API key saved on {}.", host.machine));
                }
                SetupKind::Add if provider == "dsh" => {
                    if !policy::valid_label(&request.label) {
                        return Err("Enter an account name.".into());
                    }
                    let installed = ["profiles", "removed_profiles"].iter().any(|f| {
                        catalog[*f].as_array().is_some_and(|rows| {
                            rows.iter()
                                .any(|r| r["id"] == "native-dsh" && r["installed"] == true)
                        })
                    });
                    if installed {
                        catalog = host.binding.set_key(
                            &self.0.hgs,
                            Some(request.label.trim()),
                            request.api_key.trim(),
                        )?;
                        profile = Some(format!("{}\u{1f}native-dsh", host.binding.computer.0));
                        notices.push(format!("DeepSeek API key saved on {}.", host.machine));
                    } else {
                        if let Some(note) = setup_terminal(
                            dir,
                            &self.0.hgs,
                            &host.binding.terminal_call("install", "dsh")?.args,
                        )? {
                            notices.push(note);
                        }
                        notices.push(format!("Finish installing DeepSeek on {}, then refresh Accounts and set its API key.",host.machine));
                    }
                }
                SetupKind::Add => {
                    let id = format!(
                        "{}-{}",
                        provider,
                        &uuid::Uuid::new_v4().simple().to_string()[..10]
                    );
                    catalog = host
                        .binding
                        .create(&self.0.hgs, &id, provider, &request.label)?;
                    profile = Some(format!("{}\u{1f}{id}", host.binding.computer.0));
                    if catalog["profiles"].as_array().is_some_and(|rows| {
                        rows.iter().any(|r| r["id"] == id && r["installed"] == true)
                    }) {
                        let next = HostBinding::capture(
                            host.binding.computer.clone(),
                            host.binding.machine_id.clone(),
                            &catalog,
                        )?;
                        // Creation is confirmed. A terminal failure keeps that catalog;
                        // it never authorizes another add.
                        match setup_terminal(
                            dir,
                            &self.0.hgs,
                            &next.terminal_call("login", &id)?.args,
                        ) {
                            Ok(note) => {
                                notices.extend(note);
                                notices.push(format!("Complete {} sign-in in the terminal on {}, this page updates automatically.",accounts::provider_name(provider),host.machine));
                            }
                            Err(error) => {
                                errors.push(format!("Could not open agent sign-in: {error}"))
                            }
                        }
                    } else {
                        notices.push("Account profile added. Install the agent on that machine, then sign in.".into());
                    }
                }
                SetupKind::Login | SetupKind::Install => {
                    let b = plan.selected.as_ref().ok_or(account_actions::CHANGED)?;
                    let (op, id) = if plan.kind == SetupKind::Login {
                        ("login", b.id.as_str())
                    } else {
                        ("install", provider)
                    };
                    if let Some(note) =
                        setup_terminal(dir, &self.0.hgs, &host.binding.terminal_call(op, id)?.args)?
                    {
                        notices.push(note);
                    }
                    if plan.kind == SetupKind::Login {
                        notices.push(format!("Complete {} sign-in in the terminal on {}, this page updates automatically.",accounts::provider_name(provider),host.machine));
                    }
                }
                SetupKind::Start => {
                    let b = plan.selected.as_ref().ok_or(account_actions::CHANGED)?;
                    catalog = host.binding.start(&self.0.hgs, &b.id)?;
                    notices.push(format!("OpenCode started on {}.", host.machine));
                }
            }
            Ok((
                catalog,
                SetupResult {
                    notices,
                    errors,
                    profile,
                },
            ))
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(execute))
            .unwrap_or_else(|_| Err(READ_FAILED.into()));
        self.edit_finish(
            &host.binding.computer,
            result
                .as_ref()
                .map(|(v, _)| v.clone())
                .map_err(Clone::clone),
        );
        result.map(|(_, view)| view)
    }
}
#[cfg(feature = "live")]
#[tauri::command(async)]
pub fn account_setup_open(
    state: State<'_, Screens>,
    source: State<'_, crate::commands::AppState>,
    kind: SetupKind,
    account: String,
    profile: String,
) -> Result<SetupView, String> {
    state.setup_open(&source.pump(), kind, &account, &profile)
}
#[cfg(feature = "live")]
#[tauri::command(async)]
pub fn account_setup_apply(
    app: tauri::AppHandle,
    state: State<'_, Screens>,
    source: State<'_, crate::commands::AppState>,
    request: SetupRequest,
) -> Result<SetupResult, String> {
    state.setup_apply(&source.pump(), &crate::data_dir(&app), request)
}
#[tauri::command]
pub fn account_setup_close(state: State<'_, Screens>, handle: String) {
    state
        .0
        .edits
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .setups
        .retain(|(h, _)| h != &handle);
}
#[cfg(not(feature = "live"))]
#[tauri::command]
pub fn account_setup_open() -> Result<SetupView, String> {
    Err(REPLAY_NEEDS_HGS.into())
}
#[cfg(not(feature = "live"))]
#[tauri::command]
pub fn account_setup_apply() -> Result<SetupResult, String> {
    Err(REPLAY_NEEDS_HGS.into())
}

#[cfg(test)]
mod tests {
    use super::super::tests::fixture;
    use super::*;
    fn request(handle: String) -> SetupRequest {
        SetupRequest {
            handle,
            host: String::new(),
            provider: "codex".into(),
            saved: String::new(),
            label: "Work".into(),
            api_key: String::new(),
        }
    }
    #[test]
    fn add_snapshots_are_bounded_and_an_unoffered_host_consumes_without_io() {
        let (screens, pump, _) = fixture();
        for _ in 0..12 {
            screens.setup_open(&pump, SetupKind::Add, "", "").unwrap();
        }
        assert_eq!(screens.0.edits.0.lock().unwrap().setups.len(), 8);
        let opened = screens.setup_open(&pump, SetupKind::Add, "", "").unwrap();
        assert_eq!(opened.hosts.len(), 1);
        let mut forged = request(opened.handle.clone());
        forged.host = "other".into();
        assert!(screens
            .setup_apply(&pump, std::path::Path::new("unused"), forged)
            .is_err());
        assert!(screens
            .setup_apply(
                &pump,
                std::path::Path::new("unused"),
                request(opened.handle)
            )
            .is_err());
        assert!(!screens.0.edits.0.lock().unwrap().busy);
    }
    #[test]
    fn a_stale_catalog_refuses_before_dispatch_and_releases_admission() {
        let (screens, pump, binding) = fixture();
        let opened = screens.setup_open(&pump, SetupKind::Add, "", "").unwrap();
        screens
            .lock()
            .accounts
            .catalogs
            .get_mut(&binding.computer)
            .unwrap()
            .data
            .as_mut()
            .unwrap()["revision"] = serde_json::json!("r2");
        assert!(screens
            .setup_apply(
                &pump,
                std::path::Path::new("unused"),
                request(opened.handle)
            )
            .is_err());
        assert!(!screens.0.edits.0.lock().unwrap().busy);
    }
    #[test]
    fn setup_and_catalog_edits_share_one_admission_and_validate_action_availability() {
        let (screens, pump, _) = fixture();
        let view = screens.accounts(false);
        let account = &view.accounts[0];
        assert!(
            screens
                .setup_open(
                    &pump,
                    SetupKind::Login,
                    &account.key,
                    &account.members[0].key
                )
                .is_err(),
            "uninstalled agents cannot log in"
        );
        assert!(screens
            .setup_open(
                &pump,
                SetupKind::Install,
                &account.key,
                &account.members[0].key
            )
            .is_ok());
        screens.0.edits.0.lock().unwrap().busy = true;
        assert!(screens.setup_open(&pump, SetupKind::Add, "", "").is_err());
        assert!(screens.0.edits.0.lock().unwrap().busy);
    }
}
