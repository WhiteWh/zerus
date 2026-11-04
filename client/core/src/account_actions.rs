//! Account catalog edits from AccountsPage: exact profile, machine and
//! catalog revision, one dispatch. Calls block and belong off the UI thread.
use crate::actions::Invocation;
use crate::{launch, reads, ComputerId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::Path, time::Duration};

pub const CHANGED: &str = "Accounts changed on this machine. Refresh before saving.";
pub const UPDATING: &str = "Accounts are updating. Wait for the update to finish.";
pub const MAX_MEMBERS: usize = 256;
pub mod models;
pub mod setup;
pub mod transfer;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Rename,
    Default,
    Remove,
    Permissions,
    Model,
}

#[derive(Clone, Debug, Default)]
pub struct Values {
    pub label: String,
    pub mode: String,
    pub model: String,
    pub effort: String,
}

/// Native-only evidence captured when the action opens, never read from a
/// webview-supplied path or an inferred provider identity.
#[derive(Clone, Debug)]
pub struct Binding {
    pub computer: ComputerId,
    pub machine_id: String,
    pub id: String,
    pub provider: String,
    pub home: String,
    pub auth_revision: String,
    pub revision: String,
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn profile<'a>(catalog: &'a Value, id: &str) -> Result<&'a Value, String> {
    let rows = catalog
        .get("profiles")
        .and_then(Value::as_array)
        .ok_or(CHANGED)?;
    if rows.len() > MAX_MEMBERS {
        return Err(CHANGED.into());
    }
    let mut found = rows.iter().filter(|row| text(row, "id") == id);
    let row = found.next().ok_or(CHANGED)?;
    if found.next().is_some() {
        return Err(CHANGED.into());
    }
    Ok(row)
}

impl Binding {
    pub fn capture(
        computer: ComputerId,
        machine_id: String,
        catalog: &Value,
        id: &str,
    ) -> Result<Self, String> {
        if (!computer.is_local() && !crate::fleet::valid_alias(&computer.0))
            || !crate::creation::canonical_uuid(&machine_id)
            || id.is_empty()
            || id.starts_with('-')
        {
            return Err(CHANGED.into());
        }
        let row = profile(catalog, id)?;
        let revision = text(catalog, "revision");
        if revision.is_empty() || revision.len() > 256 || text(row, "provider").is_empty() {
            return Err(CHANGED.into());
        }
        Ok(Self {
            computer,
            machine_id,
            id: id.into(),
            provider: text(row, "provider").into(),
            home: text(row, "home").into(),
            auth_revision: text(row, "auth_revision").into(),
            revision: revision.into(),
        })
    }
    pub fn verify(&self, catalog: &Value) -> Result<(), String> {
        let row = profile(catalog, &self.id)?;
        if text(catalog, "revision") != self.revision
            || text(row, "provider") != self.provider
            || text(row, "home") != self.home
            || text(row, "auth_revision") != self.auth_revision
        {
            return Err(CHANGED.into());
        }
        Ok(())
    }
    fn invocation(&self, args: &[&str]) -> Invocation {
        let mut call = Vec::new();
        if !self.computer.is_local() {
            call.push(format!("@{}", self.computer.0));
        }
        call.extend([
            "--expect-machine".into(),
            self.machine_id.clone(),
            "account".into(),
        ]);
        call.extend(args.iter().map(|s| (*s).into()));
        Invocation {
            args: call,
            stdin: None,
            timeout: Duration::from_secs(25),
            request_id: None,
        }
    }
    pub fn edit(&self, kind: Kind, label: &str) -> Result<Invocation, String> {
        self.edit_values(
            kind,
            &Values {
                label: label.into(),
                ..Default::default()
            },
        )
    }
    pub fn edit_values(&self, kind: Kind, values: &Values) -> Result<Invocation, String> {
        let op = match kind {
            Kind::Rename => "rename",
            Kind::Default => "default",
            Kind::Remove => "rm",
            Kind::Permissions => "permissions",
            Kind::Model => "model-defaults",
        };
        if kind == Kind::Default && matches!(self.provider.as_str(), "dsh" | "opencode") {
            return Err("This account has no default selection.".into());
        }
        let mut call = self.invocation(&[op, &self.id, "--revision", &self.revision]);
        if kind == Kind::Rename {
            let label = values.label.trim();
            if label.is_empty()
                || label.chars().count() > 160
                || label.chars().any(char::is_control)
            {
                return Err("Account name is required (up to 160 characters).".into());
            }
            call.args.extend(["--label".into(), label.into()]);
        }
        if kind == Kind::Permissions {
            if self.provider == "opencode"
                || !["provider", "bypass"].contains(&values.mode.as_str())
            {
                return Err("Choose provider or bypass permission mode.".into());
            }
            call.args.extend(["--mode".into(), values.mode.clone()]);
        }
        if kind == Kind::Model {
            if !["codex", "claude", "kimi"].contains(&self.provider.as_str())
                || values.model.chars().count() > 256
                || values.effort.chars().count() > 40
                || values
                    .model
                    .chars()
                    .chain(values.effort.chars())
                    .any(char::is_control)
            {
                return Err("Model defaults are unavailable for this account.".into());
            }
            call.args.extend([
                "--model".into(),
                values.model.clone(),
                "--effort".into(),
                values.effort.clone(),
            ]);
        }
        Ok(call)
    }
    /// Fresh read before the revision CAS. No uncertain exit is retried.
    pub fn execute(&self, hgs: &Path, kind: Kind, label: &str) -> Result<Value, String> {
        self.execute_values(
            hgs,
            kind,
            &Values {
                label: label.into(),
                ..Default::default()
            },
        )
    }
    pub fn fresh(&self, hgs: &Path) -> Result<Value, String> {
        let read = self.invocation(&["ls"]);
        let current = run_catalog(hgs, &read)?;
        self.verify(&current)?;
        Ok(current)
    }
    pub fn execute_values(&self, hgs: &Path, kind: Kind, values: &Values) -> Result<Value, String> {
        let edit = self.edit_values(kind, values)?;
        self.fresh(hgs)?;
        run_catalog(hgs, &edit)
    }
}

fn run_catalog(hgs: &Path, call: &Invocation) -> Result<Value, String> {
    let exit = crate::run::run_limited(hgs, call, &[], reads::READ_LIMIT)
        .map_err(|_| launch::INVALID_ACCOUNTS.to_owned())?;
    launch::accounts_reply(hgs, exit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn catalog() -> Value {
        json!({"revision":"r1","profiles":[{"id":"work","provider":"codex","home":"/accounts/example","auth_revision":"a1"}]})
    }
    fn binding() -> Binding {
        Binding::capture(
            ComputerId("studio".into()),
            "11111111-1111-4111-8111-111111111111".into(),
            &catalog(),
            "work",
        )
        .unwrap()
    }
    #[test]
    fn guards_every_call_and_keeps_labels_as_one_argument() {
        let b = binding();
        let inv = b.edit(Kind::Rename, "  Example account  ").unwrap();
        assert_eq!(
            inv.args,
            vec![
                "@studio",
                "--expect-machine",
                "11111111-1111-4111-8111-111111111111",
                "account",
                "rename",
                "work",
                "--revision",
                "r1",
                "--label",
                "Example account"
            ]
        );
        assert!(inv.stdin.is_none());
        assert_eq!(inv.timeout, Duration::from_secs(25));
        for kind in [Kind::Default, Kind::Remove] {
            assert!(b
                .edit(kind, "")
                .unwrap()
                .args
                .windows(2)
                .any(|a| a == ["--revision", "r1"]));
        }
    }
    #[test]
    fn refuses_changed_or_ambiguous_profiles_and_unbound_machines() {
        for field in ["provider", "home", "auth_revision"] {
            let mut v = catalog();
            v["profiles"][0][field] = json!("changed");
            assert!(binding().verify(&v).is_err());
        }
        let mut v = catalog();
        v["revision"] = json!("r2");
        assert!(binding().verify(&v).is_err());
        let mut v = catalog();
        let duplicate = v["profiles"][0].clone();
        v["profiles"].as_array_mut().unwrap().push(duplicate);
        assert!(binding().verify(&v).is_err());
        assert!(Binding::capture(ComputerId::local(), String::new(), &catalog(), "work").is_err());
        assert!(binding().edit(Kind::Rename, "\n").is_err());
    }
}
