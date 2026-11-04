//! The legacy editor retains its private snapshot behind one consumed handle.
use super::*;
use mobile::legacy::{self as core, service, windows};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::atomic::AtomicBool};
enum Snapshot {
    Unix(core::Snapshot),
    Windows(windows::Status),
}
pub(super) struct Plan {
    handle: String,
    snapshot: Snapshot,
    form: LegacyForm,
}
#[derive(Clone, Serialize)]
pub struct LegacyForm {
    handle: String,
    windows: bool,
    configured: bool,
    configured_relay: String,
    config: String,
    relay: String,
    directory: String,
    new_directory: String,
    status: String,
    implementation: String,
    protection: String,
    commands: Vec<(&'static str, String)>,
    running: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyRequest {
    action: String,
    edit: core::Edit,
}
#[derive(Serialize)]
pub struct LegacyReply {
    form: Option<LegacyForm>,
    notice: String,
    kind: &'static str,
}
struct Active<'a>(&'a AtomicBool);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
fn home() -> Result<PathBuf, String> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| core::PRIVATE.into())
}
impl Inner {
    fn legacy_active(&self) -> Result<Active<'_>, String> {
        if self.blocked {
            return Err(REPLAY.into());
        }
        if self.effects.swap(true, Ordering::AcqRel) {
            return Err("A mobile action is already running.".into());
        }
        Ok(Active(&self.effects))
    }
    fn legacy_form(
        &self,
        snapshot: Snapshot,
        service: service::Service,
    ) -> Result<LegacyForm, String> {
        let home = home()?;
        let handle = uuid::Uuid::new_v4().to_string();
        let (configured, relay, directory, running) = match &snapshot {
            Snapshot::Unix(s) => {
                if s.encrypted() {
                    return Err(core::ENCRYPTED.into());
                }
                (
                    s.exists(),
                    s.relay().to_owned(),
                    s.directory(&home),
                    service.status.contains("Connector service: running."),
                )
            }
            Snapshot::Windows(s) => (
                s.configured,
                s.server_url.clone(),
                PathBuf::new(),
                s.running,
            ),
        };
        let new_directory = core::new_directory(&home, &uuid::Uuid::new_v4().to_string())
            .to_string_lossy()
            .into_owned();
        let form = LegacyForm {
            handle: handle.clone(),
            windows: cfg!(windows),
            configured,
            configured_relay: if !configured {
                "not configured".into()
            } else if relay.is_empty() {
                "invalid configuration".into()
            } else {
                relay.clone()
            },
            config: mobile::config_path().to_string_lossy().into_owned(),
            relay: if relay.is_empty() {
                core::OFFICIAL.into()
            } else {
                relay
            },
            directory: if configured {
                directory.to_string_lossy().into_owned()
            } else {
                new_directory.clone()
            },
            new_directory,
            status: service.status,
            implementation: service.implementation,
            protection: service.protection,
            commands: service.commands,
            running,
        };
        *self
            .legacy_plan
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Plan {
            handle,
            snapshot,
            form: form.clone(),
        });
        Ok(form)
    }
    fn legacy_open(&self) -> Result<LegacyForm, String> {
        let _active = self.legacy_active()?;
        if self
            .page()
            .view()
            .workspace
            .as_ref()
            .is_some_and(|w| w.encrypted)
        {
            return Err(core::ENCRYPTED.into());
        }
        self.legacy_plan
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let (connector, config) = workspace_runtime();
        if cfg!(windows) {
            let s = windows::status(&connector, &config)?;
            let status = if s.running {
                "Connector: running. Relay connectivity is not verified here."
            } else {
                "Connector: stopped"
            }
            .into();
            return self.legacy_form(
                Snapshot::Windows(s),
                service::Service {
                    status,
                    ..Default::default()
                },
            );
        }
        let snapshot = core::load(&config)?;
        if snapshot.encrypted() {
            return Err(core::ENCRYPTED.into());
        }
        let (program, args, mac) = if cfg!(target_os = "macos") {
            // SAFETY: geteuid has no preconditions; launchctl print is read-only.
            #[cfg(unix)]
            let uid = unsafe { libc::geteuid() };
            #[cfg(not(unix))]
            let uid = 0;
            (
                "launchctl",
                vec![
                    "print".into(),
                    format!("gui/{uid}/com.hgdev.zerus-mobile-connector"),
                ],
                true,
            )
        } else {
            ("systemctl",vec!["--user".into(),"show".into(),"zerus-mobile-connector.service".into(),"--property=LoadState,ActiveState,SubState,KillMode,UnitFileState,FragmentPath,DropInPaths,ExecStart".into(),"--no-pager".into()],false)
        };
        let service = service::probe(std::path::Path::new(program), args, snapshot.exists(), mac);
        self.legacy_form(Snapshot::Unix(snapshot), service)
    }
    fn legacy_close(&self, handle: &str) {
        let mut plan = self
            .legacy_plan
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if plan.as_ref().is_some_and(|p| p.handle == handle) {
            plan.take();
        }
    }
    fn legacy_apply(&self, handle: &str, request: LegacyRequest) -> Result<LegacyReply, String> {
        let _active = self.legacy_active()?;
        let mut slot = self
            .legacy_plan
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !slot.as_ref().is_some_and(|p| p.handle == handle) {
            return Err(core::CHANGED.into());
        }
        let plan = slot.take().ok_or(core::CHANGED)?;
        drop(slot);
        // Consumed before parsing/admission or dispatch. An unknown result has no replay handle.
        let (connector, config) = workspace_runtime();
        let result = match &plan.snapshot {
            Snapshot::Unix(snapshot) => {
                if request.action != "save" {
                    return Err(core::CHANGED.into());
                }
                core::save(&config, snapshot, request.edit, &home()?).map(|_| core::SAVED)
            }
            Snapshot::Windows(snapshot) => {
                if mobile::read_status(&connector, &config)
                    .map_err(|_| windows::UNCONFIRMED)?
                    .encrypted
                {
                    return Err(core::ENCRYPTED.into());
                }
                let edit = if request.action == "save" {
                    Some(request.edit)
                } else {
                    None
                };
                windows::apply(&connector, &config, snapshot, &request.action, edit)
                    .map(|answer| answer.1)
            }
        };
        self.legacy_complete(plan, result)
    }
    fn legacy_complete(
        &self,
        mut plan: Plan,
        result: Result<&'static str, core::Failure>,
    ) -> Result<LegacyReply, String> {
        match result {
            Ok(notice) => Ok(LegacyReply {
                form: None,
                notice: notice.into(),
                kind: if notice.starts_with("Configuration saved privately.") {
                    "ok"
                } else {
                    "muted"
                },
            }),
            Err(core::Failure::Locked(message)) => Err(message),
            Err(core::Failure::Refused(message)) => {
                let handle = uuid::Uuid::new_v4().to_string();
                plan.handle = handle.clone();
                plan.form.handle = handle;
                let form = plan.form.clone();
                *self
                    .legacy_plan
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(plan);
                Ok(LegacyReply {
                    form: Some(form),
                    notice: message,
                    kind: "error",
                })
            }
        }
    }
}
#[tauri::command(async)]
pub fn mobile_legacy_open(mobile: State<'_, Mobile>) -> Result<LegacyForm, String> {
    mobile.0.legacy_open()
}
#[tauri::command(async)]
pub fn mobile_legacy_apply(
    mobile: State<'_, Mobile>,
    handle: String,
    request: LegacyRequest,
) -> Result<LegacyReply, String> {
    mobile.0.legacy_apply(&handle, request)
}
#[tauri::command(async)]
pub fn mobile_legacy_close(mobile: State<'_, Mobile>, handle: String) {
    mobile.0.legacy_close(&handle);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(blocked: bool) -> Inner {
        Inner::new(
            blocked,
            super::super::tests::service_calls_for_plans(),
            Box::new(|| {}),
        )
    }
    fn form(inner: &Inner) -> LegacyForm {
        inner
            .legacy_form(
                Snapshot::Unix(core::Snapshot::default()),
                service::Service::default(),
            )
            .unwrap()
    }
    #[test]
    fn one_snapshot_only_and_old_close_cannot_cancel_new_editor() {
        let i = fixture(false);
        let old = form(&i);
        let new = form(&i);
        i.legacy_close(&old.handle);
        assert_eq!(
            i.legacy_plan.lock().unwrap().as_ref().unwrap().handle,
            new.handle
        );
        i.legacy_close(&new.handle);
        assert!(i.legacy_plan.lock().unwrap().is_none());
    }
    #[test]
    fn handle_is_consumed_before_any_dispatch_or_admission_failure() {
        let i = fixture(false);
        let f = form(&i);
        let request = || LegacyRequest {
            action: "stop".into(),
            edit: core::Edit {
                relay: String::new(),
                replacement: String::new(),
                directory: PathBuf::new(),
            },
        };
        assert!(i.legacy_apply(&f.handle, request()).is_err());
        assert!(i.legacy_plan.lock().unwrap().is_none());
        assert!(i.legacy_apply(&f.handle, request()).is_err());
    }
    #[test]
    fn replay_other_mobile_effects_and_encrypted_workspace_refuse_before_io() {
        assert!(fixture(true).legacy_open().is_err());
        let i = fixture(false);
        let active = i.legacy_active().unwrap();
        assert!(i.legacy_open().is_err());
        drop(active);
        let w=mobile::workspace(serde_json::json!({"state":"active","encrypted":true,"server_url":"https://relay.example.invalid"}).as_object().unwrap());
        i.page().workspace_answered(Ok(w));
        assert_eq!(i.legacy_open().err().unwrap(), core::ENCRYPTED);
    }
    #[test]
    fn validation_rearms_the_original_snapshot_but_changed_sources_stay_locked() {
        let i = fixture(false);
        let old = form(&i);
        let plan = i.legacy_plan.lock().unwrap().take().unwrap();
        let reply = i
            .legacy_complete(
                plan,
                Err(core::Failure::Refused(
                    "Use a new receipt directory.".into(),
                )),
            )
            .unwrap();
        let fresh = reply.form.unwrap();
        assert_ne!(fresh.handle, old.handle);
        assert_eq!(fresh.relay, old.relay);
        assert_eq!(reply.kind, "error");
        let plan = i.legacy_plan.lock().unwrap().take().unwrap();
        assert_eq!(
            i.legacy_complete(plan, Err(core::Failure::Locked(core::CHANGED.into())))
                .err()
                .unwrap(),
            core::CHANGED
        );
        assert!(i.legacy_plan.lock().unwrap().is_none());
    }
}
