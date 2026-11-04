//! One explicit bounded batch on the existing Accounts writer. No replay queue.
use super::*;
use tauri::Emitter;
use zerus_client_core::account_actions::{
    setup::{valid_id, valid_label, Host as HostBinding},
    transfer::{self, Failure},
};

pub(super) struct Plan {
    provider: String,
    targets: Vec<Destination>,
    sources: Vec<Source>,
}
struct Destination {
    host: HostBinding,
    machine: String,
    saved: Option<Value>,
}
struct Source {
    machine: String,
    key: String,
    binding: Binding,
}
#[derive(Clone, Serialize)]
pub struct Machine {
    key: String,
    machine: String,
    platform: String,
    version: String,
    blocked: Option<String>,
    present: bool,
    saved: bool,
}
#[derive(Serialize)]
pub struct CopySource {
    key: String,
    label: String,
}
#[derive(Serialize)]
pub struct Form {
    handle: String,
    label: String,
    provider: String,
    machines: Vec<Machine>,
    sources: Vec<CopySource>,
}
#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Copy,
    Signin,
    Apikey,
}
#[derive(Deserialize)]
pub struct Request {
    handle: String,
    hosts: Vec<String>,
    method: Method,
    use_saved: bool,
    source: String,
    label: String,
    profile_id: String,
    #[serde(default)]
    api_key: String,
}
#[derive(Clone, Serialize)]
pub struct Progress {
    handle: String,
    host: String,
    machine: String,
    state: &'static str,
    text: String,
    saved: bool,
    profile: Option<String>,
}
impl Screens {
    fn distribution_open(
        &self,
        pump: &crate::pump::Pump,
        account: &str,
        profile: &str,
    ) -> Result<Form, String> {
        if !self.0.allowed {
            return Err(REPLAY_NEEDS_HGS.into());
        }
        let _busy = {
            let mut edits = self
                .0
                .edits
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if edits.busy {
                return Err(account_actions::UPDATING.into());
            }
            edits.busy = true;
            Busy(&self.0.edits)
        };
        let view = self.accounts(false);
        let selected = view
            .accounts
            .iter()
            .find(|a| a.key == account)
            .ok_or(account_actions::CHANGED)?;
        let member = selected
            .members
            .iter()
            .find(|m| m.key == profile)
            .ok_or(account_actions::CHANGED)?;
        if !view.binding_evidence_complete {
            return Err(account_actions::CHANGED.into());
        }
        let group = &view
            .binding_evidence
            .iter()
            .find(|e| e.host == member.host && e.id == member.id && e.home == member.home)
            .ok_or(account_actions::CHANGED)?
            .group;
        let belongs = |host: &ComputerId, row: &Value| {
            view.binding_evidence.iter().any(|e| {
                e.group == *group
                    && e.host == *host
                    && e.id == row["id"].as_str().unwrap_or("")
                    && e.home == row["home"].as_str().unwrap_or("")
            })
        };
        let identities: BTreeMap<_, _> = view
            .hosts
            .iter()
            .map(|h| (h.computer.clone(), pump.machine(&h.computer)))
            .collect();
        let hub = self.current();
        let mut machines = Vec::new();
        let mut targets = Vec::new();
        let mut sources = Vec::new();
        let mut shown_sources = Vec::new();
        for host in view.hosts.iter().take(64) {
            let meta = identities.get(&host.computer).and_then(Option::as_ref);
            let id = meta.and_then(|m| m.machine_id.clone());
            let raw = hub
                .accounts
                .catalogs
                .get(&host.computer)
                .and_then(|s| s.data.as_ref());
            let visible = raw.and_then(|v| v["profiles"].as_array());
            let removed = raw.and_then(|v| v["removed_profiles"].as_array());
            let present =
                visible.is_some_and(|rows| rows.iter().any(|r| belongs(&host.computer, r)));
            let saved = removed
                .and_then(|rows| rows.iter().find(|r| belongs(&host.computer, r)))
                .map(setup::row_evidence);
            let installed = [visible, removed]
                .into_iter()
                .flatten()
                .flatten()
                .any(|r| r["provider"] == selected.provider && r["installed"] == true);
            let entry = hub.fleet.iter().find(|e| e.computer == host.computer);
            let blocked = if present {
                Some("Already has this account".into())
            } else if !host.online {
                Some("Not connected".into())
            } else if entry.is_none_or(|e| !accounts::launchable(e, &selected.provider))
                || !installed
            {
                Some(format!(
                    "{} is not installed",
                    accounts::provider_name(&selected.provider)
                ))
            } else if selected.provider == "opencode" {
                Some("OpenCode accounts stay on this computer".into())
            } else if raw.is_none() || id.is_none() {
                Some(account_actions::UPDATING.into())
            } else {
                None
            };
            if let (Some(raw), Some(id)) = (raw, id) {
                let binding = HostBinding::capture(host.computer.clone(), id, raw)?;
                if blocked.is_none() {
                    targets.push(Destination {
                        host: binding.clone(),
                        machine: host.name.clone(),
                        saved: saved.clone(),
                    });
                }
                if host.online {
                    for row in visible.into_iter().flatten().filter(|r| {
                        belongs(&host.computer, r)
                            && r["portable"] == true
                            && r["credential_file"] == true
                    }) {
                        if sources.len() >= 256 {
                            break;
                        }
                        let source = Binding::capture(
                            host.computer.clone(),
                            binding.machine_id.clone(),
                            raw,
                            row["id"].as_str().ok_or(account_actions::CHANGED)?,
                        )?;
                        let key = format!("{}\u{1f}{}", host.computer.0, source.id);
                        shown_sources.push(CopySource {
                            key: key.clone(),
                            label: format!(
                                "{} / {}",
                                host.name,
                                row["label"].as_str().unwrap_or(&selected.label)
                            ),
                        });
                        sources.push(Source {
                            machine: host.name.clone(),
                            key,
                            binding: source,
                        });
                    }
                }
            }
            machines.push(Machine {
                key: host.computer.0.clone(),
                machine: host.name.clone(),
                platform: entry.and_then(|e| e.platform.clone()).unwrap_or_default(),
                version: meta.and_then(|m| m.hgs_version.clone()).unwrap_or_default(),
                blocked,
                present,
                saved: saved.is_some(),
            });
        }
        drop(hub);
        let handle = uuid::Uuid::new_v4().to_string();
        let answer = Form {
            handle: handle.clone(),
            label: selected.label.clone(),
            provider: selected.provider.clone(),
            machines,
            sources: shown_sources,
        };
        let mut edits = self
            .0
            .edits
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while edits.distributions.len() >= 8 {
            edits.distributions.pop_front();
        }
        edits.distributions.push_back((
            handle,
            Plan {
                provider: selected.provider.clone(),
                targets,
                sources,
            },
        ));
        Ok(answer)
    }
    fn distribution_apply(
        &self,
        pump: &crate::pump::Pump,
        dir: &std::path::Path,
        request: Request,
        emit: impl Fn(Progress),
    ) -> Result<Vec<Progress>, String> {
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
            let i = edits
                .distributions
                .iter()
                .position(|(h, _)| h == &request.handle)
                .ok_or(account_actions::CHANGED)?;
            let (_, plan) = edits
                .distributions
                .remove(i)
                .ok_or(account_actions::CHANGED)?;
            edits.busy = true;
            plan
        };
        let _busy = Busy(&self.0.edits);
        if request.hosts.is_empty()
            || request.hosts.len() > 64
            || !valid_label(&request.label)
            || !valid_id(&request.profile_id)
            || (request.method == Method::Apikey) != (plan.provider == "dsh")
            || (request.method == Method::Apikey
                && (request.api_key.trim().is_empty()
                    || request.api_key.len() > 4096
                    || request
                        .api_key
                        .trim()
                        .chars()
                        .any(|c| c.is_whitespace() || c.is_control())))
        {
            return Err("Choose machines, an account name and a valid profile ID.".into());
        }
        let mut selected = Vec::new();
        let mut seen = BTreeSet::new();
        for key in &request.hosts {
            let target = plan
                .targets
                .iter()
                .find(|t| t.host.computer.0 == *key)
                .ok_or(account_actions::CHANGED)?;
            if !seen.insert(target.host.machine_id.clone()) {
                return Err(account_actions::CHANGED.into());
            }
            selected.push(target);
        }
        let source = plan.sources.iter().find(|s| s.key == request.source);
        if request.method == Method::Copy
            && source.is_none()
            && selected
                .iter()
                .any(|t| !request.use_saved || t.saved.is_none())
        {
            return Err("Choose a source sign-in.".into());
        }
        let mut results = Vec::new();
        for target in selected {
            let saved =
                request.method != Method::Apikey && request.use_saved && target.saved.is_some();
            let mut result = Progress {
                handle: request.handle.clone(),
                host: target.host.computer.0.clone(),
                machine: target.machine.clone(),
                state: "working",
                text: "Working…".into(),
                saved,
                profile: None,
            };
            emit(result.clone());
            let outcome = (|| {
                if !pump.online(&target.host.computer.0)
                    || pump
                        .machine(&target.host.computer)
                        .and_then(|m| m.machine_id)
                        .as_deref()
                        != Some(&target.host.machine_id)
                {
                    return Err(Failure {
                        issued: false,
                        message: account_actions::CHANGED.into(),
                    });
                }
                self.catalog_begin(&target.host.computer, |v| target.host.verify(v))
                    .map_err(|message| Failure {
                        issued: false,
                        message,
                    })?;
                let execute = || {
                    let current = target.host.fresh(&self.0.hgs).map_err(|message| Failure {
                        issued: false,
                        message,
                    })?;
                    let exists = ["profiles", "removed_profiles"].iter().any(|f| {
                        current[*f]
                            .as_array()
                            .is_some_and(|rows| rows.iter().any(|r| r["id"] == request.profile_id))
                    });
                    let mut id = request.profile_id.clone();
                    let (catalog, state, text) = if saved {
                        let evidence = target.saved.as_ref().ok_or_else(|| Failure {
                            issued: false,
                            message: account_actions::CHANGED.into(),
                        })?;
                        id = evidence["id"]
                            .as_str()
                            .ok_or_else(|| Failure {
                                issued: false,
                                message: account_actions::CHANGED.into(),
                            })?
                            .into();
                        (
                            target
                                .host
                                .restore(&self.0.hgs, evidence)
                                .map_err(|message| Failure {
                                    issued: true,
                                    message,
                                })?,
                            "added",
                            "Added with the saved sign-in on that machine".to_owned(),
                        )
                    } else if request.method == Method::Apikey {
                        id = "native-dsh".into();
                        (
                            target
                                .host
                                .set_key(
                                    &self.0.hgs,
                                    Some(request.label.trim()),
                                    request.api_key.trim(),
                                )
                                .map_err(|message| Failure {
                                    issued: true,
                                    message,
                                })?,
                            "added",
                            "API key saved".to_owned(),
                        )
                    } else if exists {
                        return Err(Failure {
                            issued: false,
                            message: "The profile ID already exists. Choose another ID.".into(),
                        });
                    } else if request.method == Method::Copy {
                        let source = source.ok_or_else(|| Failure {
                            issued: false,
                            message: account_actions::CHANGED.into(),
                        })?;
                        if !pump.online(&source.binding.computer.0)
                            || pump
                                .machine(&source.binding.computer)
                                .and_then(|m| m.machine_id)
                                .as_deref()
                                != Some(&source.binding.machine_id)
                        {
                            return Err(Failure {
                                issued: false,
                                message: account_actions::CHANGED.into(),
                            });
                        }
                        (
                            transfer::copy(
                                &self.0.hgs,
                                &source.binding,
                                &target.host,
                                &id,
                                &request.label,
                            )?,
                            "added",
                            format!("Sign-in copied from {} into a new profile", source.machine),
                        )
                    } else {
                        target
                            .host
                            .terminal_call("login", &id)
                            .map_err(|message| Failure {
                                issued: false,
                                message,
                            })?;
                        let catalog = target
                            .host
                            .create(&self.0.hgs, &id, &plan.provider, &request.label)
                            .map_err(|message| Failure {
                                issued: true,
                                message,
                            })?;
                        let after = HostBinding::capture(
                            target.host.computer.clone(),
                            target.host.machine_id.clone(),
                            &catalog,
                        )
                        .map_err(|message| Failure {
                            issued: true,
                            message,
                        })?;
                        let text=match setup::setup_terminal(dir,&self.0.hgs,&after.terminal_call("login",&id).map_err(|message|Failure{issued:true,message})?.args) {Ok(_)=>"Profile created. Complete the sign-in in a terminal on that machine.".into(),Err(error)=>format!("Profile created. Could not open agent sign-in: {error}")};
                        (catalog, "needs-signin", text)
                    };
                    Ok((
                        catalog,
                        state,
                        text,
                        format!("{}\u{1f}{id}", target.host.computer.0),
                    ))
                };
                let work = std::panic::catch_unwind(std::panic::AssertUnwindSafe(execute))
                    .unwrap_or_else(|_| {
                        Err(Failure {
                            issued: true,
                            message: READ_FAILED.into(),
                        })
                    });
                self.edit_finish(
                    &target.host.computer,
                    work.as_ref()
                        .map(|(c, _, _, _)| c.clone())
                        .map_err(|f| f.message.clone()),
                );
                work
            })();
            match outcome {
                Ok((_, state, text, profile)) => {
                    result.state = state;
                    result.text = text;
                    result.profile = Some(profile);
                }
                Err(error) => {
                    result.state = if error.issued { "unknown" } else { "failed" };
                    result.text = error.message;
                }
            }
            emit(result.clone());
            results.push(result);
        }
        Ok(results)
    }
}
#[cfg(feature = "live")]
#[tauri::command(async)]
pub fn account_distribution_open(
    state: State<'_, Screens>,
    source: State<'_, crate::commands::AppState>,
    account: String,
    profile: String,
) -> Result<Form, String> {
    state.distribution_open(&source.pump(), &account, &profile)
}
#[cfg(feature = "live")]
#[tauri::command(async)]
pub fn account_distribution_apply(
    app: tauri::AppHandle,
    state: State<'_, Screens>,
    source: State<'_, crate::commands::AppState>,
    request: Request,
) -> Result<Vec<Progress>, String> {
    let dir = crate::data_dir(&app);
    state.distribution_apply(&source.pump(), &dir, request, |row| {
        let _ = app.emit("account-distribution-progress", row);
    })
}
#[tauri::command]
pub fn account_distribution_close(state: State<'_, Screens>, handle: String) {
    state
        .0
        .edits
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .distributions
        .retain(|(h, _)| h != &handle);
}
#[cfg(not(feature = "live"))]
#[tauri::command]
pub fn account_distribution_open() -> Result<Form, String> {
    Err(REPLAY_NEEDS_HGS.into())
}
#[cfg(not(feature = "live"))]
#[tauri::command]
pub fn account_distribution_apply() -> Result<Vec<Progress>, String> {
    Err(REPLAY_NEEDS_HGS.into())
}

#[cfg(test)]
mod tests {
    use super::super::tests::fixture;
    use super::*;
    fn ready() -> (Screens, crate::pump::Pump, HostBinding) {
        let (screens, pump, binding) = fixture();
        screens
            .lock()
            .accounts
            .catalogs
            .get_mut(&binding.computer)
            .unwrap()
            .data
            .as_mut()
            .unwrap()["removed_profiles"] = serde_json::json!([]);
        let host = HostBinding::capture(
            binding.computer,
            binding.machine_id,
            screens
                .lock()
                .accounts
                .catalogs
                .values()
                .next()
                .unwrap()
                .data
                .as_ref()
                .unwrap(),
        )
        .unwrap();
        (screens, pump, host)
    }
    fn request(handle: &str) -> Request {
        Request {
            handle: handle.into(),
            hosts: vec![String::new()],
            method: Method::Signin,
            use_saved: false,
            source: String::new(),
            label: "Example".into(),
            profile_id: "example-new".into(),
            api_key: String::new(),
        }
    }
    #[test]
    fn distribution_snapshots_are_bounded_and_open_does_no_effect() {
        let (screens, pump, _) = ready();
        let view = screens.accounts(false);
        let account = &view.accounts[0];
        for _ in 0..12 {
            let opened = screens
                .distribution_open(&pump, &account.key, &account.members[0].key)
                .unwrap();
            assert_eq!(
                opened.machines[0].blocked.as_deref(),
                Some("Already has this account")
            );
        }
        assert_eq!(screens.0.edits.0.lock().unwrap().distributions.len(), 8);
    }
    #[test]
    fn only_a_pre_effect_refusal_is_retryable_and_a_handle_is_consumed_once() {
        let (screens, pump, host) = ready();
        screens.0.edits.0.lock().unwrap().distributions.push_back((
            "captured".into(),
            Plan {
                provider: "codex".into(),
                targets: vec![Destination {
                    host,
                    machine: "workstation".into(),
                    saved: None,
                }],
                sources: vec![],
            },
        ));
        screens
            .lock()
            .accounts
            .catalogs
            .values_mut()
            .next()
            .unwrap()
            .data
            .as_mut()
            .unwrap()["revision"] = serde_json::json!("changed");
        let progress = screens
            .distribution_apply(
                &pump,
                std::path::Path::new("unused"),
                request("captured"),
                |_| {},
            )
            .unwrap();
        assert_eq!(progress[0].state, "failed");
        assert!(screens
            .distribution_apply(
                &pump,
                std::path::Path::new("unused"),
                request("captured"),
                |_| {}
            )
            .is_err());
        assert!(!screens.0.edits.0.lock().unwrap().busy);
    }
    #[test]
    fn unoffered_targets_and_duplicate_physical_computers_refuse_before_io() {
        let (screens, pump, host) = ready();
        screens.0.edits.0.lock().unwrap().distributions.push_back((
            "captured".into(),
            Plan {
                provider: "codex".into(),
                targets: vec![Destination {
                    host,
                    machine: "workstation".into(),
                    saved: None,
                }],
                sources: vec![],
            },
        ));
        let mut req = request("captured");
        req.hosts = vec![String::new(), String::new()];
        assert!(screens
            .distribution_apply(&pump, std::path::Path::new("unused"), req, |_| {})
            .is_err());
        assert!(!screens.0.edits.0.lock().unwrap().busy);
    }
}
