//! Explicit, resumable handover of one verified legacy connector service.
//! A reserved signal is never retried. Status never invokes the service manager.
use crate::{
    error::{ConnectorError, Result},
    private_state::Store,
    workspace_cli,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(all(target_os = "linux", feature = "test-hooks"))]
pub use linux::{test_scope, TestPaths};
const RECORD: &str = "legacy-service-handover.json";

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    pub revision: String,
    pub node_id: String,
    pub config: PathBuf,
    pub state: PathBuf,
    pub relay_binding: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u8,
    binding: Binding,
    phase: String,
    reason: String,
    #[serde(default)]
    details: Value,
    activated_revision: Option<String>,
    start_reserved: bool,
    #[serde(default)]
    signal_reserved: bool,
    #[serde(default)]
    process_exited: bool,
    #[serde(default)]
    managed_node_id: Option<String>,
    #[serde(default)]
    transfer: Option<Transfer>,
}
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Transfer {
    previous_revision: String,
    previous_node: String,
    next_node: String,
}
/// Only prepare_switch constructs this from the Store-held validated profile.
/// State includes the exact node-bound receipts path; Start never receives it.
#[cfg(target_os = "linux")]
struct RetiredSwitch {
    revision: String,
    state: PathBuf,
}
fn failure() -> ConnectorError {
    ConnectorError::new(
        "Connector service handover changed; reload its saved connection before continuing",
    )
}
fn read(store: &Store) -> Result<Option<Record>> {
    let record: Option<Record> = store
        .read(RECORD)
        .map_err(|_| failure())?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(|_| failure()))
        .transpose()?;
    if record.as_ref().is_some_and(|r| r.version != 1) {
        return Err(failure());
    }
    Ok(record)
}
fn save(store: &Store, record: &Record) -> Result<()> {
    let old = store.read(RECORD).map_err(|_| failure())?;
    let bytes = serde_json::to_vec(record).map_err(|_| failure())?;
    store
        .replace(RECORD, &Store::revision(old.as_deref()), &bytes)
        .map_err(|_| failure())?;
    Ok(())
}
fn public(record: &Record) -> Value {
    let draining = record.phase == "draining";
    json!({"phase":if draining {"draining"} else {"blocked"},"reason":record.reason,"retryable":draining})
}
/// Selects only bounded public state from an existing private record.
pub fn status(private: &Path, revision: &str, node_id: &str) -> Result<Option<Value>> {
    let Some(bytes) = workspace_cli::read_private(private, RECORD)? else {
        return Ok(None);
    };
    let record: Record = serde_json::from_slice(&bytes).map_err(|_| failure())?;
    if record.version != 1
        || record.binding.revision != revision
        || record.binding.node_id != node_id
    {
        return Ok(None);
    }
    Ok(matches!(record.phase.as_str(), "draining" | "blocked").then(|| public(&record)))
}
fn manual_stopped(record: &Record) -> bool {
    record.details.is_null()
        && !record.signal_reserved
        && record.activated_revision.is_none()
        && matches!(
            crate::fsguard::probe_lock(&record.binding.state.join(crate::journal::LOCK)),
            crate::fsguard::LockState::Free | crate::fsguard::LockState::Absent
        )
}
/// Called only by an explicitly confirmed switch or an explicit pending Check.
/// `None` means the caller may acquire and retain the old journal lock itself.
pub(crate) async fn drain(store: &Store, binding: Binding) -> Result<Option<Value>> {
    let previous = read(store)?;
    if previous
        .as_ref()
        .is_some_and(|r| r.version != 1 || r.binding != binding)
    {
        return Err(failure());
    }
    let mut record = previous.unwrap_or(Record {
        version: 1,
        binding,
        phase: "prepared".into(),
        reason: "draining".into(),
        details: Value::Null,
        activated_revision: None,
        start_reserved: false,
        signal_reserved: false,
        process_exited: false,
        managed_node_id: None,
        transfer: None,
    });
    #[cfg(not(target_os = "linux"))]
    if manual_stopped(&record) {
        record.phase = "manual".into();
        record.reason.clear();
        save(store, &record)?;
        return Ok(None);
    }
    #[cfg(target_os = "linux")]
    let outcome = match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        linux::drain(store, &mut record),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => Ok(Some(if record.signal_reserved {
            "drain-timeout"
        } else {
            "service-unconfirmed"
        })),
    };
    #[cfg(not(target_os = "linux"))]
    let outcome = Ok(Some("unsupported-platform"));
    // Linux first repairs a verified ordinary inactive unit. Failed/no unit
    // admission grants no service authority; the established manual free-lock
    // path still works if nothing was signaled or changed ambiguously.
    if !matches!(&outcome, Ok(None)) && manual_stopped(&record) {
        record.phase = "manual".into();
        record.reason.clear();
        save(store, &record)?;
        return Ok(None);
    }
    match outcome {
        Ok(None) => {
            record.phase = "stopped".into();
            record.reason.clear();
            save(store, &record)?;
            Ok(None)
        }
        Ok(Some(reason)) => {
            record.phase = if matches!(reason, "draining" | "drain-timeout") {
                "draining"
            } else {
                "blocked"
            }
            .into();
            record.reason = reason.into();
            save(store, &record)?;
            Ok(Some(public(&record)))
        }
        Err(error) => Err(error),
    }
}
/// Reserve a subsequent managed switch without changing the current association.
/// The caller then acquires the old journal lock and retains the exact pending destination.
pub(crate) async fn prepare_switch(
    store: &Store,
    config: &Path,
    revision: &str,
    previous_node: &str,
    next_node: &str,
) -> Result<()> {
    let Some(record) = read(store)? else {
        return Ok(());
    };
    if record.phase == "manual" && record.managed_node_id.is_none() {
        return Ok(());
    }
    let (_configuration, current_revision) = workspace_cli::read_config(config)?;
    if record.binding.config != config
        || record.activated_revision.as_deref() != Some(revision)
        || record.managed_node_id.as_deref() != Some(previous_node)
        || current_revision != revision
    {
        return Err(failure());
    }
    let transfer = Transfer {
        previous_revision: revision.into(),
        previous_node: previous_node.into(),
        next_node: next_node.into(),
    };
    if record.transfer.as_ref().is_some_and(|old| old != &transfer) {
        return Err(failure());
    }
    #[cfg(target_os = "linux")]
    {
        // No caller flag, HTTP status or unsigned hint can establish retirement.
        // The held Store keeps the validated current record pinned over awaits.
        // If proof is unavailable, the existing strict Stop-marker path remains.
        let retired = crate::workspace::retired_switch_profile_from_store(
            store,
            &_configuration,
            config,
            revision,
            previous_node,
            next_node,
            record.transfer.as_ref() == Some(&transfer),
        )
        .ok()
        .flatten();
        let retired = retired.map(|profile| RetiredSwitch {
            revision: revision.into(),
            state: profile.state_dir,
        });
        let mut record = record;
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            linux::switch_stopped(store, &mut record, retired.as_ref()),
        )
        .await
        .map_err(|_| failure())??;
        record.transfer = Some(transfer);
        save(store, &record)
    }
    #[cfg(not(target_os = "linux"))]
    Err(failure())
}
/// Read-only Stop admission. A cooperative exit is still unsafe under a foreign
/// supervisor; no caller may write the Stop marker before this succeeds.
pub(crate) async fn stop_preflight(
    config: &Path,
    private: &Path,
    revision: &str,
    profile: &crate::workspace::Profile,
    control: &Value,
) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let store = Store::lock(private, false).map_err(|_| failure())?;
            let binding = Binding {
                revision: revision.into(),
                node_id: profile.identity.device.device_id.clone(),
                config: config.into(),
                state: profile.state_dir.clone(),
                relay_binding: String::new(),
            };
            linux::stop_preflight(read(&store)?.as_ref(), &binding, control).await
        })
        .await
        .map_err(|_| failure())?
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (config, private, revision, profile, control);
        // There is no verified installer-written LaunchAgent admission yet.
        // Never infer safe supervision from encrypted workspace membership.
        Err(ConnectorError::new(
            "Stop is unavailable without a verified installer-managed connector service",
        ))
    }
}
/// Persist only an exact successful config CAS (or its exact pending recovery).
pub(crate) fn activated(store: &Store, node_id: &str, revision: &str) -> Result<()> {
    let Some(mut record) = read(store)? else {
        return Ok(());
    };
    if record.phase == "manual" && record.managed_node_id.is_none() {
        return Ok(());
    }
    if workspace_cli::read_config(&record.binding.config)?.1 != revision {
        return Err(failure());
    }
    if record.managed_node_id.as_deref() == Some(node_id)
        && record.activated_revision.as_deref() == Some(revision)
    {
        return Ok(());
    }
    let initial = record.activated_revision.is_none()
        && record.binding.node_id == node_id
        && record.phase == "stopped";
    let next = record.transfer.as_ref().is_some_and(|t| {
        t.next_node == node_id
            && record.managed_node_id.as_deref() == Some(t.previous_node.as_str())
            && record.activated_revision.as_deref() == Some(t.previous_revision.as_str())
    });
    if !initial && !next {
        return Err(failure());
    }
    record.activated_revision = Some(revision.into());
    record.managed_node_id = Some(node_id.into());
    record.transfer = None;
    record.start_reserved = false;
    save(store, &record)
}
/// Invoked after the workspace Store guard has been dropped, never by status.
/// One absolute deadline includes all awaited manager calls and observation.
pub(crate) async fn start(
    config: &Path,
    private: &Path,
    node_id: &str,
    revision: &str,
    failed_start_revision: Option<&str>,
) -> Result<Option<Value>> {
    // Unmanaged configurations do not gain a new mutation-lock dependency.
    if workspace_cli::read_private(private, RECORD)?.is_none() {
        if failed_start_revision.is_some() {
            return Err(failure());
        }
        return Ok(None);
    }
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let store = Store::lock(private, false).map_err(|_| failure())?;
        let Some(record) = read(&store)? else {
            if failed_start_revision.is_some() {
                return Err(failure());
            }
            return Ok(None);
        };
        if record.phase == "manual" && record.managed_node_id.is_none() {
            if failed_start_revision.is_some() {
                return Err(failure());
            }
            return Ok(None);
        }
        // A managed association never silently falls back to a detached process.
        if record.managed_node_id.as_deref() != Some(node_id)
            || record.binding.config != config
            || record.activated_revision.as_deref() != Some(revision)
            || workspace_cli::read_config(config)?.1 != revision
        {
            return Err(failure());
        }
        #[cfg(target_os = "linux")]
        let mut record = record;
        if let Some(expected) = failed_start_revision {
            #[cfg(target_os = "linux")]
            linux::recover_failed_start(&store, &mut record, expected).await?;
            #[cfg(not(target_os = "linux"))]
            {
                let _ = expected;
                return Err(failure());
            }
        }
        #[cfg(target_os = "linux")]
        let plan = linux::reserve_start(&store, &mut record).await?;
        #[cfg(target_os = "linux")]
        let expected = Store::revision(store.read(RECORD).map_err(|_| failure())?.as_deref());
        drop(store);
        #[cfg(target_os = "linux")]
        {
            let (status, observed) = linux::start(plan).await?;
            let store = Store::lock(private, false).map_err(|_| failure())?;
            if Store::revision(store.read(RECORD).map_err(|_| failure())?.as_deref()) != expected
                || workspace_cli::read_config(config)?.1 != revision
            {
                return Err(failure());
            }
            linux::observed(&store, &mut record, observed)?;
            Ok(Some(status))
        }
        #[cfg(not(target_os = "linux"))]
        Err(failure())
    })
    .await
    .map_err(|_| failure())?
}

/// Explicit fixture entry points are absent from ordinary/package builds.
#[cfg(all(target_os = "linux", feature = "test-hooks"))]
pub async fn test_drain(config: &Path, private: &Path, node_id: &str) -> Result<Option<Value>> {
    let (value, revision) = workspace_cli::read_config(config)?;
    let encoded = serde_json::to_string(&value).map_err(|_| failure())?;
    let parsed = crate::config::parse(&encoded)?;
    let legacy = crate::config::validate(
        &parsed,
        false,
        &crate::config::Environment::current().map_err(|_| failure())?,
    )?;
    let store = Store::lock(private, true).map_err(|_| failure())?;
    drain(
        &store,
        Binding {
            revision,
            node_id: node_id.into(),
            config: config.into(),
            state: legacy.state_dir,
            relay_binding: legacy.binding,
        },
    )
    .await
}
#[cfg(all(target_os = "linux", feature = "test-hooks"))]
pub fn test_activated(private: &Path, node_id: &str, revision: &str) -> Result<()> {
    activated(
        &Store::lock(private, false).map_err(|_| failure())?,
        node_id,
        revision,
    )
}
#[cfg(all(target_os = "linux", feature = "test-hooks"))]
pub async fn test_start(
    config: &Path,
    private: &Path,
    node_id: &str,
    revision: &str,
) -> Result<Option<Value>> {
    start(config, private, node_id, revision, None).await
}

#[cfg(all(target_os = "linux", feature = "test-hooks"))]
pub async fn test_failed_start_recovery(private: &Path, expected: &str) -> Result<()> {
    let store = Store::lock(private, false).map_err(|_| failure())?;
    let mut record = read(&store)?.ok_or_else(failure)?;
    linux::recover_failed_start(&store, &mut record, expected).await
}

#[cfg(all(target_os = "linux", feature = "test-hooks"))]
pub async fn test_prepare_switch(
    private: &Path,
    config: &Path,
    revision: &str,
    previous: &str,
    next: &str,
) -> Result<()> {
    prepare_switch(
        &Store::lock(private, false).map_err(|_| failure())?,
        config,
        revision,
        previous,
        next,
    )
    .await
}

#[cfg(all(target_os = "linux", feature = "test-hooks"))]
pub async fn test_reserve_start_only(private: &Path) -> Result<()> {
    let store = Store::lock(private, false).map_err(|_| failure())?;
    let mut record = read(&store)?.ok_or_else(failure)?;
    linux::reserve_start(&store, &mut record).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_is_read_only_and_does_not_adopt_a_different_pending_identity() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        let store = Store::lock(&private, true).unwrap();
        let binding = Binding {
            revision: "old".into(),
            node_id: "reserved".into(),
            config: dir.path().join("connector.json"),
            state: dir.path().join("receipts"),
            relay_binding: "binding".into(),
        };
        let record = Record {
            version: 1,
            binding,
            phase: "draining".into(),
            reason: "draining".into(),
            details: Value::Null,
            activated_revision: None,
            start_reserved: false,
            signal_reserved: false,
            process_exited: false,
            managed_node_id: None,
            transfer: None,
        };
        save(&store, &record).unwrap();
        drop(store);
        assert_eq!(
            status(&private, "old", "reserved").unwrap().unwrap(),
            json!({"phase":"draining","reason":"draining","retryable":true})
        );
        assert!(status(&private, "other", "reserved").unwrap().is_none());
        assert!(status(&private, "old", "other").unwrap().is_none());
    }
    #[test]
    fn only_matching_stopped_handover_accepts_an_activation_revision() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::lock(&dir.path().join("private"), true).unwrap();
        let mut record = Record {
            version: 1,
            binding: Binding {
                revision: "old".into(),
                node_id: "reserved".into(),
                config: dir.path().join("connector.json"),
                state: dir.path().join("receipts"),
                relay_binding: "binding".into(),
            },
            phase: "draining".into(),
            reason: "draining".into(),
            details: Value::Null,
            activated_revision: None,
            start_reserved: false,
            signal_reserved: false,
            process_exited: false,
            managed_node_id: None,
            transfer: None,
        };
        save(&store, &record).unwrap();
        assert!(activated(&store, "reserved", "new").is_err());
        record.phase = "stopped".into();
        save(&store, &record).unwrap();
        let revision =
            workspace_cli::replace_config(&record.binding.config, "", &json!({})).unwrap();
        assert!(activated(&store, "other", "new").is_err());
        activated(&store, "reserved", &revision).unwrap();
        assert_eq!(
            read(&store).unwrap().unwrap().activated_revision.as_deref(),
            Some(revision.as_str())
        );
    }
}
