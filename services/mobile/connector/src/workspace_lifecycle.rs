//! Cooperative, generation-bound connector lifecycle. Never signals/kills a
//! native process, PID, process group, tmux server or resident agent.
use crate::{
    error::{ConnectorError, Result},
    fsguard, journal,
    private_state::Store,
    runtime, workspace, workspace_cli,
};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
/// Consumers must require this exact capability before offering a running Stop.
pub const STOP_PREFLIGHT_ABI: &str = "service-preflight-v1";
const CONTROL: &str = "workspace-control.json";
const STOP: &str = "workspace-stop.json";
fn fail() -> ConnectorError {
    ConnectorError::new(
        "Connector lifecycle state is unavailable or changed; reload before continuing",
    )
}
fn record(state_dir: &Path, name: &str) -> Result<Option<Value>> {
    workspace_cli::read_private(state_dir, name)?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(|_| fail()))
        .transpose()
}
fn generation(value: &Value) -> Result<&str> {
    let generation = value["generation"].as_str().ok_or_else(fail)?;
    if value["version"] != 1
        || generation.len() != 32
        || !generation.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(fail());
    }
    Ok(generation)
}
fn replace(store: &Store, name: &str, value: &Value) -> Result<()> {
    let expected = Store::revision(store.read(name).map_err(|_| fail())?.as_deref());
    let bytes = serde_json::to_vec(value).map_err(|_| fail())?;
    store.replace(name, &expected, &bytes).map_err(|_| fail())?;
    Ok(())
}
/// Call only after the connector owns the exact receipt directory journal lock.
pub fn publish(state_dir: &Path) -> Result<String> {
    let store = Store::lock(state_dir, false).map_err(|_| fail())?;
    let generation = crate::private_state::random_id().map_err(|_| fail())?;
    replace(
        &store,
        CONTROL,
        &json!({"version":1,"generation":generation,"pid":std::process::id()}),
    )?;
    Ok(generation)
}
/// Reads only the marker for this process generation. An old stop record cannot
/// stop a replacement connector, even when it reuses the same receipt directory.
pub async fn monitor(stopping: &runtime::Event, state_dir: &Path, expected_generation: &str) {
    while !stopping.is_set() {
        if let Ok(Some(stop)) = record(state_dir, STOP) {
            if generation(&stop).ok() == Some(expected_generation) {
                stopping.set();
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
pub fn status(state_dir: &Path) -> Result<Value> {
    if !state_dir.exists() {
        return Ok(json!({"state":"stopped","running":false,"generation":null}));
    }
    fsguard::check_private_directory(state_dir).map_err(|_| fail())?;
    let lock = fsguard::probe_lock(&state_dir.join(journal::LOCK));
    match lock {
        fsguard::LockState::Absent | fsguard::LockState::Free => {
            Ok(json!({"state":"stopped","running":false,"generation":null}))
        }
        fsguard::LockState::Unknown => Err(fail()),
        fsguard::LockState::Held => {
            let control = record(state_dir, CONTROL)?.ok_or_else(fail)?;
            let current = generation(&control)?;
            let stopping = record(state_dir, STOP)?
                .as_ref()
                .is_some_and(|stop| generation(stop).ok() == Some(current));
            Ok(
                json!({"state":if stopping{"stopping"}else{"running"},"running":true,"generation":current}),
            )
        }
    }
}
fn configured(config_path: &Path, input: &Value) -> Result<(workspace::Profile, String, PathBuf)> {
    let (config, revision) = workspace_cli::read_config(config_path)?;
    if input["expected_revision"].as_str() != Some(&revision) {
        return Err(fail());
    }
    let marker = config["encrypted_workspace"]
        .as_object()
        .filter(|m| m.len() == 2)
        .ok_or_else(fail)?;
    if marker.get("version") != Some(&json!(1)) || config.get("node_token").is_some() {
        return Err(fail());
    }
    let private_dir = PathBuf::from(
        marker
            .get("state_dir")
            .and_then(Value::as_str)
            .ok_or_else(fail)?,
    );
    if !private_dir.is_absolute() {
        return Err(fail());
    }
    if config_path
        .parent()
        .map(|parent| parent.join("workspace-private"))
        .as_ref()
        != Some(&private_dir)
    {
        return Err(fail());
    }
    // A verified retired identity may still own a draining journal. Stop must
    // remain available for its exact generation; Start separately requires an
    // active identity, as do authenticated workspace mutations.
    let profile = workspace::load_record(&private_dir)?;
    workspace::validate_config_profile_binding(&config, &private_dir, &profile)?;
    if config["server_url"] != profile.server_url || config["state_dir"] != json!(profile.state_dir)
    {
        return Err(fail());
    }
    Ok((profile, revision, private_dir))
}
pub async fn manage_start(config_path: &Path, input: &Value) -> Result<Value> {
    let (profile, revision, private_dir) = configured(config_path, input)?;
    profile.validate()?;
    let _guard = Store::lock(&private_dir.join("lifecycle"), true).map_err(|_| fail())?;
    if workspace_cli::read_config(config_path)?.1 != revision {
        return Err(fail());
    }
    let current = status(&profile.state_dir)?;
    if current["running"] == true {
        return Ok(current);
    }
    let previous = if profile.state_dir.exists() {
        record(&profile.state_dir, CONTROL)?
    } else {
        None
    };
    let previous_generation = previous
        .as_ref()
        .and_then(|v| generation(v).ok())
        .map(str::to_owned);
    let executable = std::env::current_exe().map_err(|_| fail())?;
    let mut command = Command::new(executable);
    command
        .arg("--admit-revision")
        .arg(&revision)
        .arg("--config")
        .arg(config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().map_err(|_| fail())?;
    for _ in 0..100 {
        if child.try_wait().map_err(|_| fail())?.is_some() {
            return Err(ConnectorError::new(
                "Connector could not start; verify private configuration and journal availability",
            ));
        }
        if workspace_cli::read_config(config_path)?.1 != revision {
            return Err(fail());
        }
        if let Ok(status) = status(&profile.state_dir) {
            if status["running"] == true
                && status["generation"].as_str() != previous_generation.as_deref()
            {
                return Ok(status);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(ConnectorError::new(
        "Connector startup is still in progress; refresh its status",
    ))
}
pub async fn manage_stop(config_path: &Path, input: &Value) -> Result<Value> {
    let (profile, revision, private_dir) = configured(config_path, input)?;
    let current = status(&profile.state_dir)?;
    if current["running"] != true {
        return Ok(current);
    }
    let expected = current["generation"].as_str().ok_or_else(fail)?;
    if input
        .get("generation")
        .is_some_and(|value| value.as_str() != Some(expected))
    {
        return Err(fail());
    }
    {
        let store = Store::lock(&profile.state_dir, false).map_err(|_| fail())?;
        let control = record(&profile.state_dir, CONTROL)?.ok_or_else(fail)?;
        if generation(&control)? != expected
            || control["pid"].as_u64() == Some(std::process::id() as u64)
            || workspace_cli::read_config(config_path)?.1 != revision
        {
            return Err(fail());
        }
        crate::workspace_service::stop_preflight(
            config_path,
            &private_dir,
            &revision,
            &profile,
            &control,
        )
        .await?;
        // The admission awaited bounded manager reads. Do not publish against a
        // replaced generation, lock owner or configuration after that await.
        if record(&profile.state_dir, CONTROL)?.as_ref() != Some(&control)
            || workspace_cli::read_config(config_path)?.1 != revision
            || status(&profile.state_dir)?["generation"].as_str() != Some(expected)
        {
            return Err(fail());
        }
        replace(&store, STOP, &json!({"version":1,"generation":expected}))?;
    }
    for _ in 0..200 {
        let current = status(&profile.state_dir)?;
        if current["running"] != true {
            return Ok(current);
        }
        if current["generation"].as_str() != Some(expected) {
            return Err(fail());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(json!({"state":"stopping","running":true,"generation":expected}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn stale_stop_generation_does_not_stop_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("receipts");
        crate::fsguard::create_directory(&state).unwrap();
        let first = publish(&state).unwrap();
        let store = Store::lock(&state, false).unwrap();
        replace(&store, STOP, &json!({"version":1,"generation":first})).unwrap();
        drop(store);
        let replacement = publish(&state).unwrap();
        assert_ne!(first, replacement);
        let stopping = runtime::Event::default();
        assert!(tokio::time::timeout(
            Duration::from_millis(300),
            monitor(&stopping, &state, &replacement)
        )
        .await
        .is_err());
        assert!(!stopping.is_set());
        let store = Store::lock(&state, false).unwrap();
        replace(&store, STOP, &json!({"version":1,"generation":replacement})).unwrap();
        drop(store);
        tokio::time::timeout(
            Duration::from_secs(1),
            monitor(&stopping, &state, &replacement),
        )
        .await
        .unwrap();
        assert!(stopping.is_set());
    }
    #[test]
    fn status_does_not_create_state_or_accept_control_without_lock() {
        let directory = tempfile::tempdir().unwrap();
        let absent = directory.path().join("absent");
        assert_eq!(status(&absent).unwrap()["state"], "stopped");
        assert!(!absent.exists());
        crate::fsguard::create_directory(&absent).unwrap();
        publish(&absent).unwrap();
        assert_eq!(status(&absent).unwrap()["state"], "stopped");
        let lock = crate::fsguard::private_file(&absent.join(journal::LOCK), true)
            .unwrap()
            .unwrap();
        crate::fsguard::lock_exclusive(&lock).unwrap();
        assert_eq!(status(&absent).unwrap()["state"], "running");
        drop(lock);
        assert_eq!(status(&absent).unwrap()["state"], "stopped");
    }
}
