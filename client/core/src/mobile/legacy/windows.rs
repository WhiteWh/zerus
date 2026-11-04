//! WindowsMobileSettings: the bundled connector owns ACLs and lifecycle.
use super::*;
use crate::actions::{Exit, Invocation};
use serde::Serialize;
use serde_json::json;
use std::time::Duration;
pub const UNCONFIRMED:&str="Connector operation was not confirmed. Reload status before retrying. Check that the portable ZIP includes zerus-mobile-connector.exe and private configuration is available.";
#[derive(Clone, PartialEq, Serialize)]
pub struct Status {
    pub configured: bool,
    pub running: bool,
    pub revision: String,
    pub generation: Option<String>,
    pub server_url: String,
    pub state_dir: Option<String>,
}
fn hex(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn parse(v: Value) -> Result<Status, String> {
    let configured = v["configured"].as_bool().ok_or(UNCONFIRMED)?;
    let running = v["running"].as_bool().ok_or(UNCONFIRMED)?;
    let revision = v["revision"].as_str().ok_or(UNCONFIRMED)?.to_owned();
    let generation = v["generation"].as_str().map(str::to_owned);
    let server_url = v["server_url"].as_str().unwrap_or("").to_owned();
    if v.get("ok").is_some_and(|x| x != true)
        || v["startup_at_login"] != false
        || v.get("stopping").is_some()
        || (configured && (!hex(&revision, 64) || !super::super::valid_relay(&server_url)))
        || (!configured && (running || !revision.is_empty() || !server_url.is_empty()))
        || (running && !generation.as_deref().is_some_and(|s| hex(s, 32)))
        || (!running && !v["generation"].is_null())
    {
        return Err(UNCONFIRMED.into());
    }
    let state_dir = match &v["state_dir"] {
        Value::Null => None,
        Value::String(s) if s.len() <= 4096 => Some(s.clone()),
        _ => return Err(UNCONFIRMED.into()),
    };
    Ok(Status {
        configured,
        running,
        revision,
        generation,
        server_url,
        state_dir,
    })
}
fn call(connector: &Path, config: &Path, action: &str, payload: Value) -> Result<Value, String> {
    let invocation = Invocation {
        args: vec![
            "--manage".into(),
            action.into(),
            "--config".into(),
            config.to_string_lossy().into_owned(),
        ],
        stdin: Some(serde_json::to_vec(&payload).map_err(|_| UNCONFIRMED)?),
        timeout: Duration::from_secs(12),
        request_id: None,
    };
    let answer = crate::run::run_without(
        connector,
        &invocation,
        &[],
        &super::super::CONNECTOR_UNSET,
        65536,
    )
    .map_err(|_| UNCONFIRMED)?;
    match answer {
        Exit::Code {
            code: 0, stdout, ..
        } => serde_json::from_slice(&stdout).map_err(|_| UNCONFIRMED.into()),
        _ => Err(UNCONFIRMED.into()),
    }
}
pub fn status(connector: &Path, config: &Path) -> Result<Status, String> {
    parse(call(connector, config, "status", json!({}))?)
}
pub fn apply(
    connector: &Path,
    config: &Path,
    old: &Status,
    action: &str,
    edit: Option<Edit>,
) -> Result<(Status, &'static str), Failure> {
    if status(connector, config)? != *old {
        return Err(CHANGED.into());
    }
    let mut payload = json!({"revision":old.revision});
    let relay = if let Some(edit) = edit {
        if action != "save" || old.running {
            return Err(CHANGED.into());
        }
        let relay = edit.relay.trim_end_matches('/');
        if !super::super::valid_relay(relay) {
            return Err(Failure::Refused(
                "Enter an HTTPS relay URL without credentials, query or fragment.".into(),
            ));
        }
        if !edit.replacement.is_empty() && !token(&edit.replacement) {
            return Err(Failure::Refused(
                "Supply the computer credential issued by this relay.".into(),
            ));
        }
        payload["server_url"] = json!(relay);
        payload["node_token"] = json!(edit.replacement);
        Some(relay.to_owned())
    } else {
        None
    };
    match action {
        "save" if relay.is_some() => {}
        "start" if old.configured && !old.running => {}
        "stop" if old.running => {
            payload["generation"] = json!(old.generation);
        }
        _ => return Err(CHANGED.into()),
    }
    let reply = call(connector, config, action, payload)?;
    if action == "stop"
        && reply["stopping"] == true
        && reply["generation"] == json!(old.generation)
        && reply.as_object().is_some_and(|o| o.len() == 2)
    {
        return Ok((old.clone(),"Stop requested. The current phone request will finish before the connector exits. Reload status to confirm."));
    }
    let result = parse(reply)?;
    let confirmed = if action == "save" {
        result.configured && !result.running && Some(&result.server_url) == relay.as_ref()
    } else {
        result.configured
            && result.revision == old.revision
            && result.server_url == old.server_url
            && result.state_dir == old.state_dir
            && result.running == (action == "start")
    };
    if !confirmed {
        return Err(UNCONFIRMED.into());
    }
    Ok((
        result,
        if action == "save" {
            "Configuration saved privately. Start the connector, then refresh Phones below."
        } else {
            ""
        },
    ))
}
