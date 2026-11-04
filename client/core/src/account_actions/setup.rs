//! Account setup uses the same catalog revision and physical-computer guard.
use super::{run_catalog, Binding, CHANGED, MAX_MEMBERS};
use crate::{actions::Invocation, ComputerId};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Host {
    pub computer: ComputerId,
    pub machine_id: String,
    pub revision: String,
    pub setup_revision: bool,
}
impl Host {
    pub fn capture(
        computer: ComputerId,
        machine_id: String,
        catalog: &Value,
    ) -> Result<Self, String> {
        if (!computer.is_local() && !crate::fleet::valid_alias(&computer.0))
            || !crate::creation::canonical_uuid(&machine_id)
            || catalog["revision"]
                .as_str()
                .is_none_or(|r| r.is_empty() || r.len() > 256)
            || catalog["profiles"]
                .as_array()
                .is_none_or(|a| a.len() > MAX_MEMBERS)
            || catalog["removed_profiles"]
                .as_array()
                .is_some_and(|a| a.len() > MAX_MEMBERS)
        {
            return Err(CHANGED.into());
        }
        Ok(Self {
            computer,
            machine_id,
            revision: catalog["revision"].as_str().unwrap().into(),
            setup_revision: catalog["setup_revision"] == true,
        })
    }
    pub fn verify(&self, catalog: &Value) -> Result<(), String> {
        if catalog["revision"] != self.revision
            || (catalog["setup_revision"] == true) != self.setup_revision
        {
            return Err(CHANGED.into());
        }
        Ok(())
    }
    fn invocation(&self, args: &[&str]) -> Invocation {
        // Reuse the existing argument quoting/physical identity boundary.
        Binding {
            computer: self.computer.clone(),
            machine_id: self.machine_id.clone(),
            revision: self.revision.clone(),
            id: String::new(),
            provider: String::new(),
            home: String::new(),
            auth_revision: String::new(),
        }
        .invocation(args)
    }
    fn call(&self, op: &str, id: &str) -> Invocation {
        self.invocation(&[op, id, "--revision", &self.revision])
    }
    pub fn terminal_call(&self, op: &str, id: &str) -> Result<Invocation, String> {
        if !self.setup_revision {
            return Err("Update hgs on this machine to continue verified account setup.".into());
        }
        Ok(self.call(op, id))
    }
    pub fn fresh(&self, hgs: &Path) -> Result<Value, String> {
        let catalog = self.read(hgs)?;
        self.verify(&catalog)?;
        Ok(catalog)
    }
    pub fn read(&self, hgs: &Path) -> Result<Value, String> {
        let call = self.invocation(&["ls"]);
        run_catalog(hgs, &call)
    }
    pub fn create(
        &self,
        hgs: &Path,
        id: &str,
        provider: &str,
        label: &str,
    ) -> Result<Value, String> {
        if !["codex", "claude", "kimi"].contains(&provider) || !valid_id(id) || !valid_label(label)
        {
            return Err("Enter a valid account name and agent.".into());
        }
        let catalog = self.fresh(hgs)?;
        if ["profiles", "removed_profiles"].iter().any(|f| {
            catalog[*f]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|r| r["id"] == id))
        }) {
            return Err(CHANGED.into());
        }
        let mut call = self.call("add", id);
        call.args.extend([
            "--provider".into(),
            provider.into(),
            "--label".into(),
            label.trim().into(),
        ]);
        run_catalog(hgs, &call)
    }
    pub fn restore(&self, hgs: &Path, saved: &Value) -> Result<Value, String> {
        let current = self.fresh(hgs)?;
        let id = saved["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or(CHANGED)?;
        let mut matches = current["removed_profiles"]
            .as_array()
            .ok_or(CHANGED)?
            .iter()
            .filter(|r| r["id"] == id);
        let row = matches.next().ok_or(CHANGED)?;
        if matches.next().is_some()
            || ["provider", "home", "auth_revision"]
                .iter()
                .any(|f| row[*f] != saved[*f])
        {
            return Err(CHANGED.into());
        }
        run_catalog(hgs, &self.call("restore", id))
    }
    pub fn start(&self, hgs: &Path, id: &str) -> Result<Value, String> {
        if id != "native-opencode" {
            return Err(CHANGED.into());
        }
        self.fresh(hgs)?;
        let exit =
            crate::run::run_limited(hgs, &self.terminal_call("start", id)?, &[], 4 * 1024 * 1024)
                .map_err(|_| CHANGED)?;
        match exit {
            crate::Exit::Code { code: 0, .. } => self.fresh(hgs),
            other => Err(crate::actions::failure_detail(other)),
        }
    }
    pub fn set_key(&self, hgs: &Path, label: Option<&str>, key: &str) -> Result<Value, String> {
        if key.is_empty()
            || key.len() > 4096
            || key.chars().any(|c| c.is_control() || c.is_whitespace())
            || label.is_some_and(|s| !valid_label(s))
        {
            return Err("Enter a non-empty API key without spaces or line breaks.".into());
        }
        let current = self.fresh(hgs)?;
        if !["profiles", "removed_profiles"].iter().any(|f| {
            current[*f].as_array().is_some_and(|rows| {
                rows.iter()
                    .any(|r| r["id"] == "native-dsh" && r["installed"] == true)
            })
        }) {
            return Err("Install DeepSeek on this machine before saving its API key.".into());
        }
        let mut call = self.call("set-key", "native-dsh");
        call.args.push("--json".into());
        let mut value = json!({"api_key":key});
        if let Some(label) = label {
            value["label"] = json!(label);
        }
        call.stdin = Some(serde_json::to_vec(&value).map_err(|_| CHANGED)?);
        run_catalog(hgs, &call)
    }
}
pub fn valid_label(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty() && s.chars().count() <= 160 && !s.chars().any(char::is_control)
}
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 48
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && s.as_bytes()[0].is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn setup_keeps_physical_identity_and_revision_and_validates_inputs() {
        let h = Host::capture(
            ComputerId("studio".into()),
            "11111111-1111-4111-8111-111111111111".into(),
            &json!({"revision":"r1","profiles":[]}),
        )
        .unwrap();
        assert_eq!(
            h.call("login", "example").args,
            [
                "@studio",
                "--expect-machine",
                "11111111-1111-4111-8111-111111111111",
                "account",
                "login",
                "example",
                "--revision",
                "r1"
            ]
        );
        assert!(h.verify(&json!({"revision":"r2"})).is_err());
        assert!(h.terminal_call("login", "example").is_err());
        let mut supported = h.clone();
        supported.setup_revision = true;
        assert!(supported.terminal_call("login", "example").is_ok());
        assert!(valid_label(" Work "));
        assert!(!valid_label("bad\nname"));
        assert!(valid_id("codex-example"));
        assert!(!valid_id("--other"));
    }
}
