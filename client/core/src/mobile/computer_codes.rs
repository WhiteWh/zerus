//! Explicit workspace computer invitations. Secrets use stdin/stdout only.
use super::{read_status, valid_relay, Workspace};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

pub const CHANGED: &str = "This mobile connection changed. Refresh status before continuing.";
pub const UNCONFIRMED: &str = super::WORKSPACE_UNCONFIRMED;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub config_revision: String,
    pub server_url: String,
}
impl Target {
    pub fn capture(workspace: &Workspace) -> Result<Self, String> {
        if workspace.state != "active"
            || !workspace.encrypted
            || workspace.pending.is_some()
            || workspace.config_revision.len() != 64
            || !workspace
                .config_revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !valid_relay(&workspace.server_url)
        {
            return Err(CHANGED.into());
        }
        Ok(Self {
            config_revision: workspace.config_revision.clone(),
            server_url: workspace.server_url.clone(),
        })
    }
    fn fresh(&self, connector: &Path, config: &Path) -> Result<(), String> {
        let current = read_status(connector, config).map_err(|_| CHANGED)?;
        if Self::capture(&current)? != *self {
            return Err(CHANGED.into());
        }
        Ok(())
    }
    fn call(
        &self,
        connector: &Path,
        config: &Path,
        action: &str,
        mut payload: Value,
    ) -> Result<Value, String> {
        self.fresh(connector, config)?;
        payload["expected_revision"] = json!(self.config_revision);
        super::workspace_request(connector, config, action, &payload)
    }
}

/// A transient credential. No Debug, settings, shared page or event storage.
#[derive(Serialize)]
pub struct Invitation {
    pub code: String,
    pub invitation_id: String,
    pub expires_at: f64,
}
pub fn invite(connector: &Path, config: &Path, target: &Target) -> Result<Invitation, String> {
    let value = target.call(connector, config, "invite-computer", json!({}))?;
    let code = value["code"]
        .as_str()
        .filter(|c| {
            c.starts_with("ze1.")
                && c.len() > 4
                && c.len() <= 4096
                && c[4..]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        })
        .ok_or(UNCONFIRMED)?;
    let id = value["invitation_id"]
        .as_str()
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        .ok_or(UNCONFIRMED)?;
    let expires = value["expires_at"]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.0)
        .ok_or(UNCONFIRMED)?;
    if value["purpose"] != "computer" || value["server_url"] != target.server_url {
        return Err(UNCONFIRMED.into());
    }
    Ok(Invitation {
        code: code.into(),
        invitation_id: id.into(),
        expires_at: expires,
    })
}
pub fn cancel(
    connector: &Path,
    config: &Path,
    target: &Target,
    invitation_id: &str,
) -> Result<(), String> {
    if invitation_id.is_empty()
        || invitation_id.len() > 128
        || !invitation_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(CHANGED.into());
    }
    let reply = target.call(
        connector,
        config,
        "cancel-invite",
        json!({"invitation_id": invitation_id}),
    )?;
    if reply["cancelled"] != true {
        return Err(UNCONFIRMED.into());
    }
    Ok(())
}
pub fn check(connector: &Path, config: &Path, target: &Target) -> Result<Workspace, String> {
    let reply = target.call(connector, config, "check", json!({}))?;
    let value = reply
        .as_object()
        .filter(|value| super::status_shape(value))
        .ok_or(UNCONFIRMED)?;
    Ok(super::workspace(value))
}
