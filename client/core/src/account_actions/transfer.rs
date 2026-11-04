//! Existing hgs credential transport; credentials never pass through the UI.
use super::{
    setup::{valid_id, valid_label, Host},
    Binding, CHANGED,
};
use crate::{
    actions::{failure_detail, Exit, Invocation},
    run::run_limited,
};
use serde_json::Value;
use std::{path::Path, time::Duration};
pub const MARKER: &str = "Account transfer ABI: revision-copy-v1";
#[derive(Debug)]
pub struct Failure {
    pub issued: bool,
    pub message: String,
}
impl Failure {
    fn before(message: impl Into<String>) -> Self {
        Self {
            issued: false,
            message: message.into(),
        }
    }
    fn after(message: impl Into<String>) -> Self {
        Self {
            issued: true,
            message: message.into(),
        }
    }
}
pub fn copy(
    hgs: &Path,
    source: &Binding,
    destination: &Host,
    id: &str,
    label: &str,
) -> Result<Value, Failure> {
    if !valid_id(id)
        || !valid_label(label)
        || !["codex", "kimi"].contains(&source.provider.as_str())
        || source.auth_revision.is_empty()
    {
        return Err(Failure::before(
            "Choose a portable sign-in, valid profile ID and account name.",
        ));
    }
    let current = source.fresh(hgs).map_err(Failure::before)?;
    let target = destination.fresh(hgs).map_err(Failure::before)?;
    if current["copy_revision"] != true || target["copy_revision"] != true {
        return Err(Failure::before(
            "Update hgs on both machines before copying an account.",
        ));
    }
    let row = current["profiles"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["id"] == source.id))
        .ok_or_else(|| Failure::before(CHANGED))?;
    if row["portable"] != true || row["credential_file"] != true {
        return Err(Failure::before(
            "No portable credentials were found. Sign in on the destination machine.",
        ));
    }
    let help = Invocation {
        args: vec!["--help".into()],
        stdin: None,
        timeout: Duration::from_secs(5),
        request_id: None,
    };
    let help = run_limited(hgs, &help, &[], 64 * 1024)
        .map_err(|_| Failure::before("Could not verify account transfer support."))?;
    if !matches!(help,Exit::Code{code:0,ref stdout,..} if String::from_utf8_lossy(stdout).lines().any(|s|s==MARKER))
    {
        return Err(Failure::before(
            "Update the local hgs before copying an account.",
        ));
    }
    let route = |s: &str| {
        if s.is_empty() {
            "@local".to_owned()
        } else {
            s.to_owned()
        }
    };
    let args = [
        "account",
        "copy",
        &source.id,
        "--from",
        &route(&source.computer.0),
        "--to",
        &route(&destination.computer.0),
        "--as",
        id,
        "--label",
        label.trim(),
        "--source-machine",
        &source.machine_id,
        "--destination-machine",
        &destination.machine_id,
        "--source-revision",
        &source.revision,
        "--destination-revision",
        &destination.revision,
        "--source-auth-revision",
        &source.auth_revision,
    ]
    .map(str::to_owned)
    .to_vec();
    let call = Invocation {
        args,
        stdin: None,
        timeout: Duration::from_secs(90),
        request_id: None,
    };
    let exit = run_limited(hgs, &call, &[], 8 * 1024 * 1024).map_err(|_| {
        Failure::after("The copy result is unknown. Check the destination before trying again.")
    })?;
    let receipt = match exit {
        Exit::Code {
            code: 0, stdout, ..
        } => serde_json::from_slice::<Value>(&stdout)
            .map_err(|_| Failure::after("Invalid account copy receipt."))?,
        other => return Err(Failure::after(failure_detail(other))),
    };
    if receipt["ok"] != true || receipt["id"] != id || receipt["provider"] != source.provider {
        return Err(Failure::after("Invalid account copy receipt."));
    }
    let catalog = destination.read(hgs).map_err(Failure::after)?;
    let copied = Binding::capture(
        destination.computer.clone(),
        destination.machine_id.clone(),
        &catalog,
        id,
    )
    .map_err(Failure::after)?;
    if copied.provider != source.provider {
        return Err(Failure::after(CHANGED));
    }
    Ok(catalog)
}
