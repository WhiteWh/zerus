//! One private, replaceable observation; never read by ownership admission.
use super::View;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Connecting,
    WorkerStarted,
    FrontendRequestPrepared,
    StartupGrant,
    RetryGrant,
    Rearm,
    Result,
    ConnectionClosed,
}

pub(super) fn record(phase: Phase, view: &View, first: Option<(&'static str, &'static str)>) {
    let absolute = |key| {
        std::env::var_os(key)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    if let Some(base) =
        absolute("XDG_STATE_HOME").or_else(|| absolute("HOME").map(|p| p.join(".local/state")))
    {
        let _ = write_record(
            &base.join("hgs/client-ownership-diagnostic.json"),
            phase,
            view,
            first,
        );
    }
}

fn code(value: &str) -> &str {
    // Root receipts carry a bounded String. Only known codes, never arbitrary
    // service/child text, may reach this diagnostic. The UI View stays intact.
    const KNOWN: &[&str] = &[
        "",
        "incompatible_ownership_cli",
        "ownership_identity_unavailable",
        "ownership_transport_busy",
        "ownership_stopped",
        "ownership_mode_mismatch",
        "ownership_transport_unavailable",
        "invalid_ownership_response",
        "ownership_unsupported",
        "invalid_ownership_query",
        "invalid_ownership_status",
        "ownership_fresh_native_takeover",
        "ownership_not_applied",
        "ownership_unconfirmed",
        "ownership_configuration_changed",
        "ownership_rearm_required",
        "invalid_ownership_request",
        "ownership_receipt_mismatch",
        "ownership_changed",
        "ownership_takeover_unconfirmed",
        "ownership_resources_pending",
        "ownership_drain_pending",
        "ownership_pending",
        "ownership_read_refused",
        "ownership_revoked",
        "ownership_scope_changed",
        "paired_desktop_cli_unavailable",
    ];
    if KNOWN.contains(&value) {
        value
    } else {
        "unrecognized_code"
    }
}

fn write_record(
    path: &Path,
    phase: Phase,
    view: &View,
    first: Option<(&str, &str)>,
) -> std::io::Result<()> {
    use std::io::Write;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_millis();
    let request_id = view
        .request_id
        .as_deref()
        .filter(|value| uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == *value));
    let status = view.status.as_ref().map(|status| serde_json::json!({
        "state": status.state, "revision": status.revision, "identity_current": status.identity_current
    }));
    let body = serde_json::to_vec(&serde_json::json!({
        "version": 1, "recorded_at_unix_ms": timestamp, "phase": phase,
        "request_id": request_id, "pending": view.pending, "running": view.running,
        "code": code(&view.code), "status": status,
        "first_failure": first.map(|(step, code)| serde_json::json!({"step":step,"code":code}))
    }))?;
    if body.len() > 1024 {
        return Err(std::io::Error::other("ownership diagnostic exceeds limit"));
    }
    let folder = path
        .parent()
        .ok_or_else(|| std::io::Error::other("missing diagnostic directory"))?;
    let mut directories = std::fs::DirBuilder::new();
    directories.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directories.mode(0o700);
    }
    directories.create(folder)?;
    let temporary = folder.join(format!(
        ".ownership-observation-{}.tmp",
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&body)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn record_projects_only_private_bounded_fields_and_replaces_without_following() {
        let root =
            std::env::temp_dir().join(format!("ownership-observation-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("record.json");
        let unrelated = root.join("unrelated");
        std::fs::write(&unrelated, "kept").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&unrelated, &path).unwrap();
        let request = uuid::Uuid::new_v4().to_string();
        let mut view = View {
            request_id: Some(request.clone()),
            pending: true,
            running: true,
            code: "/private/untrusted-child-secret".into(),
            status: Some(
                serde_json::from_value(json!({
                    "abi":"desktop-ownership-v1","configuration":"private-configuration-secret",
                    "journal":"private-journal-secret","revision":7,"state":"preparing",
                    "rearm_required":false,"request_id":"private-status-request-secret",
                    "owner_epoch":"private-owner-secret","identity_current":false
                }))
                .unwrap(),
            ),
        };
        write_record(
            &path,
            Phase::FrontendRequestPrepared,
            &view,
            Some(("native_takeover.frontend_request", "child_failed")),
        )
        .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() <= 1024);
        assert!(!String::from_utf8_lossy(&bytes).contains("secret"));
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            value
                .as_object_mut()
                .unwrap()
                .remove("recorded_at_unix_ms")
                .unwrap()
                .as_u64()
                .unwrap()
                > 0
        );
        assert_eq!(
            value,
            json!({"version":1,"phase":"frontend_request_prepared","request_id":request,
            "pending":true,"running":true,"code":"unrecognized_code",
            "status":{"state":"preparing","revision":7,"identity_current":false},
            "first_failure":{"step":"native_takeover.frontend_request","code":"child_failed"}})
        );
        assert_eq!(std::fs::read_to_string(&unrelated).unwrap(), "kept");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(write_record(
            &path,
            Phase::Result,
            &view,
            Some((&"x".repeat(1025), "child_failed"))
        )
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        view.request_id = Some(uuid::Uuid::new_v4().to_string());
        view.code.clear();
        view.pending = false;
        view.running = false;
        write_record(&path, Phase::Result, &view, None).unwrap();
        let next: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_ne!(next["request_id"], value["request_id"]);
        assert!(next["first_failure"].is_null());
        assert_eq!(next["pending"], false);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }
}
