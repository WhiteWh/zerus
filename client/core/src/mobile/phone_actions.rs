//! Explicit phone actions on the workspace/relay shown in Phones. Never replay.
use super::{phones, valid_relay, PhoneList};
use crate::actions::{Exit, Invocation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::Path, time::Duration};

pub const CHANGED: &str = "This mobile connection changed. Refresh the phone list and try again.";
pub const UNCERTAIN: &str =
    "The mobile action result is unconfirmed. Refresh Phones before trying again.";
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub host: String,
    pub workspace_id: String,
    pub workspace_name: String,
    pub server_url: String,
    pub encrypted: bool,
}
impl Target {
    pub fn capture(list: &PhoneList) -> Result<Self, String> {
        if list.workspace_id.is_empty()
            || list.workspace_id.len() > 64
            || !list
                .workspace_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || !valid_relay(&list.server_url)
            || list.host.len() > 256
        {
            return Err(CHANGED.into());
        }
        Ok(Self {
            host: list.host.clone(),
            workspace_id: list.workspace_id.clone(),
            workspace_name: list.workspace_name.clone(),
            server_url: list.server_url.trim_end_matches('/').into(),
            encrypted: list.encrypted,
        })
    }
    fn fresh(&self, hgs: &Path) -> Result<PhoneList, String> {
        let current = phones(hgs, &self.host)
            .map_err(|_| CHANGED)?
            .ok_or(CHANGED)?;
        let now = Self::capture(&current)?;
        if now.host != self.host
            || now.workspace_id != self.workspace_id
            || now.server_url != self.server_url
            || now.encrypted != self.encrypted
        {
            return Err(CHANGED.into());
        }
        Ok(current)
    }
    fn call(&self, action: &str, phone: Option<&str>) -> Invocation {
        let mut args = Vec::new();
        if !self.host.is_empty() {
            args.push(format!("@{}", self.host));
        }
        args.extend(["mobile".into(), action.into()]);
        if let Some(phone) = phone {
            args.push(phone.into());
        }
        args.extend([
            "--workspace".into(),
            self.workspace_id.clone(),
            "--server".into(),
            self.server_url.clone(),
            "--json".into(),
        ]);
        Invocation {
            args,
            stdin: None,
            timeout: Duration::from_secs(40),
            request_id: None,
        }
    }
}
/// Credentials remain transient; deliberately no Debug implementation.
#[derive(Serialize)]
pub struct Invitation {
    pub pair_code: String,
    pub pair_url: String,
    pub expires_at: f64,
    pub workspace_name: String,
    pub encrypted: bool,
    pub server_url: String,
}
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn answer(hgs: &Path, call: Invocation) -> Result<Value, String> {
    // Never surface a CLI payload, stderr or provider error: it can contain a code.
    let output = crate::run::run_limited(hgs, &call, &[], 64 * 1024).map_err(|_| UNCERTAIN)?;
    match output {
        Exit::Code {
            code: 0, stdout, ..
        } => serde_json::from_slice(&stdout).map_err(|_| UNCERTAIN.into()),
        _ => Err(UNCERTAIN.into()),
    }
}
pub fn invite(hgs: &Path, target: &Target) -> Result<Invitation, String> {
    target.fresh(hgs)?;
    let value = answer(hgs, target.call("invite", None))?;
    let code = value["pair_code"]
        .as_str()
        .filter(|c| !c.is_empty() && c.len() <= 16 * 1024 && !c.chars().any(char::is_control))
        .ok_or("The relay returned an invalid pairing code.")?;
    let link = value["pair_url"]
        .as_str()
        .ok_or("The relay returned an invalid pairing code.")?;
    if value["workspace_id"] != target.workspace_id
        || value["server_url"]
            .as_str()
            .map(|s| s.trim_end_matches('/'))
            != Some(&target.server_url)
    {
        return Err("This pairing code belongs to another workspace or relay than the one listed. Refresh the phone list and try again.".into());
    }
    if link
        != format!(
            "zerus://pair?server={}&code={}",
            encode(&target.server_url),
            encode(code)
        )
    {
        return Err("The relay returned an invalid pairing code.".into());
    }
    let expires = value["expires_at"]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.0)
        .ok_or("The relay returned an invalid pairing code.")?;
    Ok(Invitation {
        pair_code: code.into(),
        pair_url: link.into(),
        expires_at: expires,
        workspace_name: value["workspace_name"]
            .as_str()
            .unwrap_or(&target.workspace_name)
            .chars()
            .take(160)
            .collect(),
        encrypted: value["encrypted"] == true,
        server_url: target.server_url.clone(),
    })
}
pub fn revoke(hgs: &Path, target: &Target, phone: &str) -> Result<(), String> {
    let current = target.fresh(hgs)?;
    if phone.len() != 36
        || !phone.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
        || !current.phones.iter().any(|p| p.id == phone)
    {
        return Err(CHANGED.into());
    }
    let receipt = answer(hgs, target.call("revoke-phone", Some(phone)))?;
    if receipt["revoked"] != true || receipt["device_id"] != phone {
        return Err(UNCERTAIN.into());
    }
    Ok(())
}
