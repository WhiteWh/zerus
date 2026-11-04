//! Explicit per-user native registration, isolated in this exact CLI image.
use super::registration::trace;
use super::{registration, store::Location};
use crate::config::Config;
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::{ffi::OsString, os::windows::ffi::OsStringExt, time::Duration};
use zerus_client_core::actions::{Exit, Invocation};

mod native;
const CAP: usize = super::windows_registration_policy::CAP;
const ENTRY: &str = "__desktop-registration-native";
const ABI: &str = "desktop-native-registration-v1";

pub(super) fn entry(args: &[String]) -> Option<i32> {
    (args.first().map(String::as_str) == Some(ENTRY)).then(|| native::serve(&args[1..]))
}

fn helper_environment(trace_path: &str) -> Result<Vec<(String, String)>> {
    let mut path = [0u16; 32768];
    let length = unsafe {
        windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(
            path.as_mut_ptr(),
            path.len() as u32,
        )
    } as usize;
    ensure!(
        length > 0 && length < path.len(),
        "Windows system directory unavailable"
    );
    let system = std::path::PathBuf::from(OsString::from_wide(&path[..length]));
    let windows = system.parent().context("Windows directory unavailable")?;
    let temp = std::env::temp_dir();
    let mut result = Vec::new();
    for (key, path) in [
        ("SystemRoot", windows.to_path_buf()),
        ("WINDIR", windows.to_path_buf()),
        ("PATH", system.clone()),
        ("TEMP", temp.clone()),
        ("TMP", temp),
    ] {
        result.push((
            key.into(),
            path.to_str()
                .context("Windows helper path is not Unicode")?
                .to_owned(),
        ));
    }
    result.push((
        "HGS_DESKTOP_REGISTRATION_TRACE_FILE".into(),
        trace_path.into(),
    ));
    Ok(result)
}
fn invocation(operation: &str, plan: &Value) -> Result<Invocation> {
    ensure!(
        matches!(operation, "check" | "status" | "install" | "remove"),
        "invalid registration operation"
    );
    let request = serde_json::to_vec(&json!({"schema":1,"operation":operation,"plan":plan}))?;
    ensure!(
        !request.is_empty() && request.len() <= CAP,
        "desktop registration request exceeds its bound"
    );
    Ok(Invocation {
        args: vec![ENTRY.into(), request.len().to_string()],
        stdin: Some(request),
        timeout: Duration::from_secs(15),
        request_id: None,
    })
}
fn exchange(operation: &str, plan: &Value) -> Result<Value> {
    let invocation = invocation(operation, plan)?;
    // The selected service CLI may be a maintained copy or custom selector.
    // The implementation worker is always this original caller's own image.
    let program = std::env::current_exe()?;
    let trace = trace::Trace::new();
    let environment = helper_environment(trace.path().unwrap_or(""))?;
    let environment: Vec<_> = environment
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let result =
        zerus_client_core::run::run_hidden_limited(&program, &invocation, &environment, CAP);
    trace.finish();
    let result = result.map_err(|error| {
        use zerus_client_core::run::HiddenError;
        match error {
            HiddenError::OutputTooLarge(_) => anyhow::anyhow!("desktop registration helper output exceeded its bound; inspect status before retrying"),
            HiddenError::TerminationPending { child } => anyhow::anyhow!("desktop registration helper termination is pending (original pid {}, creation FILETIME {:?}); outcome unavailable, preserve its identity and inspect before retrying", child.pid, child.creation_filetime),
            HiddenError::PreviousExchangePending { child } => anyhow::anyhow!("desktop registration previous helper is still unsettled ({child:?}); no new helper was started"),
        }
    })?;
    let (code, stdout) = match result {
        Exit::Code { code, stdout, .. } => (code, stdout),
        _ => bail!("desktop registration outcome is unavailable; inspect status before retrying"),
    };
    let envelope: Value = serde_json::from_slice(&stdout)
        .context("invalid native registration receipt; inspect status before retrying")?;
    ensure!(
        envelope["abi"] == ABI,
        "native registration implementation changed; inspect status before retrying"
    );
    if code != 0 {
        bail!(
            "desktop registration refused: {}",
            envelope["error"]
                .as_str()
                .unwrap_or("helper failed; inspect status before retrying")
        );
    }
    let value = envelope["reply"].clone();
    ensure!(
        value["schema"] == 1
            && value["operation"] == operation
            && value["configuration"] == plan["configuration"]
            && value["task_name"] == plan["task_name"]
            && value["registered"].is_boolean()
            && value["changed"].is_boolean(),
        "desktop registration receipt mismatch; inspect status before retrying"
    );
    Ok(value)
}

pub(super) fn command(config: &Config, location: &Location, operation: &str) -> Result<Value> {
    ensure!(
        matches!(operation, "install" | "remove" | "status"),
        "invalid desktop registration operation"
    );
    let mut plan = registration::windows_plan(config, location)?;
    // Complete the finite request preflight and reject foreign registrations
    // before changing the stable CLI. Registration itself never starts a task.
    let checked = exchange(
        if operation == "install" {
            "check"
        } else {
            "status"
        },
        &plan,
    )?;
    if operation == "status" {
        return Ok(checked);
    }
    let explicit = std::env::var_os("HGS_EXECUTABLE").is_some_and(|value| !value.is_empty());
    let mut staged = None;
    if operation == "install" && !explicit {
        let layout = crate::host::staging::Layout::new(&config.home, &config.dir);
        let cli = crate::host::staging::install_cli(&layout, &std::env::current_exe()?)?;
        staged = Some(json!({"path":cli.path,"sha256":cli.sha256,"changed":cli.changed}));
        plan = registration::windows_plan(config, location)?;
    }
    let mut result = exchange(operation, &plan)?;
    result["started"] = false.into();
    result["stopped"] = false.into();
    if let Some(cli) = staged {
        result["cli"] = cli;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_worker_uses_fixed_argv_and_the_original_framed_request() {
        let plan =
            json!({"argv":["C:/Example/CLI & tools/hgs.exe"],"configuration":"a".repeat(64)});
        let inv = invocation("status", &plan).unwrap();
        assert_eq!(
            inv.args,
            [ENTRY, &inv.stdin.as_ref().unwrap().len().to_string()]
        );
        assert_eq!(
            serde_json::from_slice::<Value>(inv.stdin.as_ref().unwrap()).unwrap()["plan"],
            plan
        );
        assert_eq!(inv.timeout, Duration::from_secs(15));
        assert!(invocation("start", &plan).is_err());
        assert!(invocation("install", &json!({"oversize":"x".repeat(CAP)})).is_err());
    }
}
