//! Explicit Machines setup through the existing bounded CLI runner. No replay.
use super::*;
use serde::Deserialize;
use zerus_client_core::{
    actions::{Exit, Invocation},
    machine_connections::{self, Reference},
};

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Install {
        reference: Reference,
        source: String,
    },
    Enable {
        machine_id: String,
    },
}
#[derive(Serialize)]
pub struct Outcome {
    pub success: bool,
    pub uncertain: bool,
    pub text: String,
    pub output: String,
}
const OUTPUT: usize = 256 * 1024;
fn outcome(exit: Result<Exit, zerus_client_core::TooLarge>, enable: bool) -> Outcome {
    match exit {
        Ok(Exit::Code {
            code: 0,
            stdout,
            stderr,
        }) => {
            let text = if enable {
                match serde_json::from_slice::<Value>(&stdout) {
                    Ok(value) if value["enabled"] == true => "Local sessions are enabled on this computer. Zerus runs the stable HGS CLI from its next start.".to_owned(),
                    _ => return unknown(String::from_utf8_lossy(&stdout).into_owned() + &stderr),
                }
            } else {
                "HGS installed. Checking connection…".into()
            };
            Outcome {
                success: true,
                uncertain: false,
                text,
                output: bounded(String::from_utf8_lossy(&stdout).into_owned() + &stderr),
            }
        }
        Ok(Exit::Code {
            code,
            stdout,
            stderr,
        }) => Outcome {
            success: false,
            uncertain: true,
            text: format!(
                "Setup result is unknown (exit {code}). Check the setup output before trying again."
            ),
            output: bounded(String::from_utf8_lossy(&stdout).into_owned() + &stderr),
        },
        Ok(Exit::StartFailed(error)) => Outcome {
            success: false,
            uncertain: false,
            text: error,
            output: String::new(),
        },
        Ok(Exit::Abnormal { stderr }) => unknown(stderr),
        Ok(Exit::TimedOut) => unknown("Setup timed out.".into()),
        Ok(Exit::WriteFailed) => unknown("Setup input failed.".into()),
        Err(error) => unknown(error.stderr),
    }
}
fn bounded(text: String) -> String {
    text.chars().take(64 * 1024).collect()
}
fn unknown(output: String) -> Outcome {
    Outcome {
        success: false,
        uncertain: true,
        text: "Setup result is unknown. Check the machine and setup output before trying again."
            .into(),
        output: bounded(output),
    }
}
impl Screens {
    pub(super) fn setup_machine(
        &self,
        request: Request,
        local: Option<zerus_client_core::machine::MachineMeta>,
    ) -> Result<Outcome, String> {
        if !self.0.allowed {
            return Err(REPLAY_NEEDS_HGS.into());
        }
        let enable = matches!(&request, Request::Enable { .. });
        {
            let mut hub = self.lock();
            if let Request::Install { reference, source } = &request {
                machine_connections::setup(
                    hub.machines
                        .list
                        .as_ref()
                        .ok_or(machine_connections::CHANGED)?,
                    reference,
                    source,
                )?;
            } else if let Request::Enable { machine_id } = &request {
                let local = local.as_ref().ok_or(machine_connections::CHANGED)?;
                if machine_id.is_empty()
                    || local.machine_id.as_ref() != Some(machine_id)
                    || !zerus_client_core::launch::can_enable_local_sessions(local)
                {
                    return Err("Local session host changed. Refresh Machines before enabling local sessions.".into());
                }
            }
            if !hub.machines.start_write() {
                return Err("A machine request is already running.".into());
            }
            hub.dirty.insert(Screen::Machines);
        }
        let _work = MachineWork(self.clone());
        self.publish();
        let call = match request {
            Request::Install { reference, source } => {
                let fresh = message(reads::read(
                    &self.0.hgs,
                    &ComputerId::local(),
                    &Read::Machines,
                ))?;
                machine_connections::setup(&fresh, &reference, &source)?
            }
            Request::Enable { .. } => {
                let status = Invocation {
                    args: vec!["session-host".into(), "status".into(), "--json".into()],
                    stdin: None,
                    timeout: Duration::from_secs(5),
                    request_id: None,
                };
                let status = zerus_client_core::run_limited(&self.0.hgs, &status, &[], 16 * 1024)
                    .map_err(|_| "Could not check the local session host.")?;
                let value = match status {
                    Exit::Code {
                        code: 0, stdout, ..
                    } => serde_json::from_slice::<Value>(&stdout)
                        .map_err(|_| "Could not check the local session host.")?,
                    _ => return Err("Could not check the local session host.".into()),
                };
                if value["enabled"] != false {
                    return Err("Local session host changed. Refresh Machines before enabling local sessions.".into());
                }
                Invocation {
                    args: vec!["session-host".into(), "install".into(), "--json".into()],
                    stdin: None,
                    timeout: Duration::from_secs(60),
                    request_id: None,
                }
            }
        };
        Ok(outcome(
            zerus_client_core::run_limited(&self.0.hgs, &call, &[], OUTPUT),
            enable,
        ))
    }
}
#[tauri::command]
pub async fn machine_setup(
    state: State<'_, Screens>,
    app_state: State<'_, crate::commands::AppState>,
    request: Request,
) -> Result<Outcome, String> {
    if crate::actions::blocked() {
        return Err(REPLAY_NEEDS_HGS.into());
    }
    let local = app_state.pump().machine(&ComputerId::local());
    let screens = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || screens.setup_machine(request, local))
        .await
        .map_err(|error| error.to_string())?
}
#[tauri::command]
pub async fn machine_pick_source(app: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    if crate::actions::blocked() {
        return Err(REPLAY_NEEDS_HGS.into());
    }
    let window = crate::main_window(&app).ok_or("The main window is unavailable.")?;
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("HGS source folder")
            .set_parent(&window)
            .blocking_pick_folder()
    })
    .await
    .map_err(|error| error.to_string())?;
    let Some(file) = picked else {
        return Ok(None);
    };
    let path = file
        .into_path()
        .map_err(|_| "Choose a local HGS source folder.")?;
    let text = path.to_str().ok_or("Choose a local HGS source folder.")?;
    if !path.is_absolute() || text.len() > 4096 || text.chars().any(char::is_control) {
        return Err("Choose an absolute HGS source folder path.".into());
    }
    Ok(Some(text.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn setup_success_requires_zero_exit_and_valid_enable_receipt() {
        let code = |body: &[u8]| {
            Ok(Exit::Code {
                code: 0,
                stdout: body.to_vec(),
                stderr: String::new(),
            })
        };
        assert!(outcome(code(b"setup complete"), false).success);
        assert!(outcome(code(br#"{"enabled":true}"#), true).success);
        for body in [b"{}".as_slice(), br#"{"enabled":false}"#, b"malformed"] {
            let result = outcome(code(body), true);
            assert!(!result.success && result.uncertain);
        }
    }
    #[test]
    fn unknown_or_partial_results_never_become_success_and_output_is_bounded() {
        for exit in [
            Exit::TimedOut,
            Exit::WriteFailed,
            Exit::Abnormal {
                stderr: "unknown".into(),
            },
            Exit::Code {
                code: 75,
                stdout: b"partial".to_vec(),
                stderr: String::new(),
            },
        ] {
            let result = outcome(Ok(exit), false);
            assert!(!result.success && result.uncertain);
        }
        assert_eq!(bounded("x".repeat(100_000)).len(), 64 * 1024);
    }
}
