//! Workspace enrollment through the existing connector, never a second writer.
use super::{
    read_status, valid_relay, workspace_request, Workspace, WORKSPACE_UNCONFIRMED as UNKNOWN,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
pub const CHANGED: &str = "This mobile connection changed. Refresh status before continuing.";
pub const DRAINING:&str="The current phone request is still draining. Refresh status, then continue the workspace change.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub state: String,
    pub encrypted: bool,
    pub config_revision: String,
    pub server_url: String,
    pub node_id: String,
    pub running: bool,
    pub stop_supported: bool,
    pub generation: Option<String>,
    pub pending_state: Option<String>,
    pub pending_node_id: Option<String>,
    pub pending_action: Option<String>,
}
impl Target {
    pub fn capture(w: &Workspace) -> Result<Self, String> {
        if !matches!(
            w.state.as_str(),
            "active" | "retired" | "legacy" | "unconfigured" | "pending-activation"
        ) || (!w.config_revision.is_empty()
            && (w.config_revision.len() != 64
                || !w
                    .config_revision
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))))
            || (w.state != "unconfigured" && w.config_revision.is_empty())
            || w.server_url.len() > 2048
            || w.node_id.len() > 128
            || w.pending_node_id
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.len() > 128)
            || w.pending_state.as_ref().is_some_and(|v| {
                !matches!(
                    v.as_str(),
                    "pending"
                        | "ready"
                        | "legacy-retirement-unconfirmed"
                        | "migration-draining"
                        | "migration-blocked"
                )
            })
            || (w.pending.is_some()
                && (w.pending_state.is_none()
                    || w.pending_node_id.is_none()
                    || w.pending_action.is_none()))
            || w.pending_action.as_ref().is_some_and(|v| {
                !matches!(
                    v.as_str(),
                    "create" | "join" | "switch-create" | "switch-join"
                )
            })
        {
            return Err(CHANGED.into());
        }
        Ok(Self {
            state: w.state.clone(),
            encrypted: w.encrypted,
            config_revision: w.config_revision.clone(),
            server_url: w.server_url.clone(),
            node_id: w.node_id.clone(),
            running: w.running,
            stop_supported: w.stop_supported,
            generation: w.generation.clone(),
            pending_state: w.pending_state.clone(),
            pending_node_id: w.pending_node_id.clone(),
            pending_action: w.pending_action.clone(),
        })
    }
    pub(super) fn fresh(&self, connector: &Path, config: &Path) -> Result<Workspace, String> {
        let current = read_status(connector, config).map_err(|_| CHANGED)?;
        if Self::capture(&current)? != *self {
            return Err(CHANGED.into());
        }
        Ok(current)
    }
    pub fn changing(&self) -> bool {
        matches!(self.state.as_str(), "active" | "retired" | "legacy")
    }
    pub fn can_enroll(&self) -> bool {
        self.pending_state.is_none()
            && self.state != "pending-activation"
            && (!self.running || self.stop_supported)
    }
    pub fn can_check(&self) -> bool {
        self.pending_state.is_some() && self.pending_node_id.is_some()
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Create,
    Join,
}
/// Contains an invitation credential; no Debug, Serialize or persistence.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub mode: Mode,
    pub relay: String,
    #[serde(default)]
    pub code: String,
}
impl Request {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_relay(self.relay.trim()) {
            return Err("Enter an HTTPS relay URL without credentials, query or fragment.".into());
        }
        if self.mode == Mode::Join
            && (!self.code.starts_with("ze1.")
                || self.code.len() <= 4
                || self.code.len() > 4096
                || !self.code[4..]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)))
        {
            return Err("Enter a computer invitation code from an enrolled computer.".into());
        }
        if self.mode == Mode::Create && !self.code.is_empty() {
            return Err(CHANGED.into());
        }
        Ok(())
    }
}
pub struct Answer {
    pub workspace: Workspace,
    pub notice: &'static str,
    pub continue_node: Option<String>,
    pub connected: bool,
}
fn result(
    connector: &Path,
    config: &Path,
    target: &Target,
    value: Value,
    relay: Option<&str>,
) -> Result<Answer, String> {
    let state = value["state"].as_str().ok_or(UNKNOWN)?;
    let node = value["node_id"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 128)
        .ok_or(UNKNOWN)?;
    let active = state == "active";
    if active {
        let revision = value["config_revision"].as_str().ok_or(UNKNOWN)?;
        if value["encrypted"] != true
            || revision.len() != 64
            || !revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(UNKNOWN.into());
        }
    } else if !matches!(
        state,
        "pending" | "legacy-retirement-unconfirmed" | "migration-draining" | "migration-blocked"
    ) || value["config_revision"] != target.config_revision
    {
        return Err(UNKNOWN.into());
    }
    if state.starts_with("migration-")
        && (value["encrypted"] != false || !super::migration_shape(&value["migration"], state))
    {
        return Err(UNKNOWN.into());
    }
    let workspace = read_status(connector, config).map_err(|_| UNKNOWN)?;
    if active {
        if workspace.state != "active"
            || !workspace.encrypted
            || workspace.node_id != node
            || value["config_revision"] != workspace.config_revision
        {
            return Err(UNKNOWN.into());
        }
        if relay.is_some_and(|relay| workspace.server_url != relay) {
            return Err(UNKNOWN.into());
        }
    } else if workspace.config_revision != target.config_revision
        || workspace.pending_node_id.as_deref() != Some(node)
    {
        return Err(UNKNOWN.into());
    }
    let notice=match state {
        "migration-draining"=>"Finishing the current phone request before switching. Local agents keep running.",
        "migration-blocked"=>match value["migration"]["reason"].as_str(){Some("unsupported-runtime")=>"The existing connector needs an update before this switch can continue. Your saved connection is kept.",Some("unsafe-unit"|"service-definition-conflict")=>"The existing connector's service settings do not allow a safe automatic switch. Your saved connection is kept.",_=>"The connector switch could not be confirmed. Your saved connection is kept; refresh its status before continuing."},
        "pending"=>"Waiting for the inviting computer. Keep it online, then check this connection.",
        "legacy-retirement-unconfirmed"=>"The old relay has not confirmed retirement. The new connection is saved; your current configuration is unchanged.",
        _ if value["legacy_retirement"]=="unconfirmed"=>"The encrypted workspace is connected. Legacy relay retirement remains unconfirmed; its old records or credentials may remain on that relay.",
        _=>"Workspace connected.",
    };
    let continue_node = (state == "migration-draining"
        && workspace.pending_state.as_deref() == Some("migration-draining")
        && value["migration"]["retryable"] == true)
        .then(|| node.to_owned());
    Ok(Answer {
        workspace,
        notice,
        continue_node,
        connected: active,
    })
}
pub fn enroll(
    connector: &Path,
    config: &Path,
    target: &Target,
    request: &Request,
    computer: &str,
) -> Result<Answer, String> {
    request.validate()?;
    if !target.can_enroll() {
        return Err(CHANGED.into());
    }
    target.fresh(connector, config)?;
    let mut expected = target.clone();
    if target.changing() && target.encrypted && target.running {
        let generation = target.generation.as_deref().ok_or(CHANGED)?;
        let stopped = workspace_request(
            connector,
            config,
            "stop",
            &json!({"expected_revision":target.config_revision,"generation":generation}),
        )?;
        if stopped["state"] == "stopping"
            && stopped["running"] == true
            && stopped["generation"] == generation
        {
            let workspace = read_status(connector, config).map_err(|_| UNKNOWN)?;
            let observed = Target::capture(&workspace).map_err(|_| UNKNOWN)?;
            let mut stopped_target = target.clone();
            stopped_target.running = false;
            stopped_target.generation = None;
            if observed != *target && observed != stopped_target {
                return Err(UNKNOWN.into());
            }
            return Ok(Answer {
                workspace,
                notice: DRAINING,
                continue_node: None,
                connected: false,
            });
        }
        if stopped["state"] != "stopped"
            || stopped["running"] != false
            || !stopped["generation"].is_null()
        {
            return Err(UNKNOWN.into());
        }
        expected.running = false;
        expected.generation = None;
    }
    expected.fresh(connector, config)?;
    let action = match (target.changing(), request.mode) {
        (false, Mode::Create) => "create",
        (false, Mode::Join) => "join",
        (true, Mode::Create) => "switch-create",
        (true, Mode::Join) => "switch-join",
    };
    let computer = computer.trim().chars().take(128).collect::<String>();
    let mut payload = json!({"expected_revision":target.config_revision,"server_url":request.relay.trim().trim_end_matches('/'),"workspace_name":"Workspace","computer_name":if computer.is_empty(){"Computer"}else{&computer}});
    if request.mode == Mode::Join {
        payload["code"] = json!(request.code);
    }
    if target.state == "legacy" {
        payload["allow_legacy_service_handover"] = json!(true);
    }
    let value = workspace_request(connector, config, action, &payload)?;
    result(
        connector,
        config,
        &expected,
        value,
        Some(request.relay.trim().trim_end_matches('/')),
    )
}
pub fn check(
    connector: &Path,
    config: &Path,
    target: &Target,
    allow_local: bool,
) -> Result<Answer, String> {
    if !target.can_check()
        || (allow_local && target.pending_state.as_deref() != Some("legacy-retirement-unconfirmed"))
    {
        return Err(CHANGED.into());
    }
    target.fresh(connector, config)?;
    let mut payload = json!({"expected_revision":target.config_revision});
    if target.state == "legacy" {
        payload["allow_legacy_service_handover"] = json!(true);
    }
    if allow_local {
        payload["allow_unconfirmed_legacy_retirement"] = json!(true);
    }
    let value = workspace_request(connector, config, "check", &payload)?;
    result(connector, config, target, value, None)
}
