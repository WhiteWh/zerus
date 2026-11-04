//! Closed, bounded account model-settings projection. Read only on open.
use super::{Binding, Values, CHANGED};
use crate::actions::{failure_detail, Exit};
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeSet, path::Path};
pub const INVALID: &str = "Invalid account model settings response";
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Defaults {
    pub model: String,
    pub effort: String,
}
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Model {
    pub id: String,
    pub label: String,
    pub effort_options: Vec<String>,
}
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Catalog {
    pub models: Vec<Model>,
    pub defaults: Defaults,
    pub provider_defaults: Defaults,
    pub effort_supported: bool,
}
fn short(value: &Value, limit: usize) -> String {
    value
        .as_str()
        .filter(|s| s.chars().count() <= limit && !s.chars().any(char::is_control))
        .unwrap_or("")
        .into()
}
fn defaults(value: &Value) -> Defaults {
    Defaults {
        model: short(&value["model"], 256),
        effort: short(&value["effort"], 40),
    }
}
impl Catalog {
    pub fn parse(binding: &Binding, value: &Value) -> Result<Self, String> {
        if value["account_id"] != binding.id
            || value["provider"] != binding.provider
            || value["revision"] != binding.revision
        {
            return Err(CHANGED.into());
        }
        let rows = value["models"].as_array().ok_or(INVALID)?;
        let mut seen = BTreeSet::new();
        let mut models = Vec::new();
        for row in rows.iter().take(256) {
            let id = short(&row["id"], 256);
            if id.is_empty() || !seen.insert(id.clone()) {
                continue;
            }
            let label = short(&row["label"], 160);
            let mut efforts = Vec::new();
            for value in row["effort_options"]
                .as_array()
                .into_iter()
                .flatten()
                .take(64)
            {
                let value = short(value, 40);
                if !value.is_empty() && !efforts.contains(&value) {
                    efforts.push(value);
                }
            }
            models.push(Model {
                id: id.clone(),
                label: if label.is_empty() { id } else { label },
                effort_options: efforts,
            });
        }
        Ok(Self {
            models,
            defaults: defaults(&value["defaults"]),
            provider_defaults: defaults(&value["provider_defaults"]),
            effort_supported: value["effort_supported"] == true,
        })
    }
    pub fn validate(&self, values: &Values) -> Result<(), String> {
        if values.model.is_empty() {
            return if values.effort.is_empty() {
                Ok(())
            } else {
                Err("Choose a model before setting effort.".into())
            };
        }
        let chosen=self.models.iter().find(|m|m.id==values.model).ok_or("This model is absent from the current account catalog. Choose another model or Provider settings.")?;
        if !values.effort.is_empty()
            && (!self.effort_supported || !chosen.effort_options.contains(&values.effort))
        {
            return Err("Effort is unavailable for the selected model.".into());
        }
        Ok(())
    }
}
impl Binding {
    pub fn model_settings(&self, hgs: &Path) -> Result<Catalog, String> {
        self.fresh(hgs)?;
        let call = self.invocation(&["model-settings", &self.id, "--json"]);
        let exit = crate::run::run_limited(hgs, &call, &[], 4 * 1024 * 1024)
            .map_err(|_| INVALID.to_owned())?;
        let value = match exit {
            Exit::Code {
                code: 0, stdout, ..
            } => serde_json::from_slice::<Value>(&stdout).map_err(|_| INVALID.to_owned())?,
            other => return Err(failure_detail(other)),
        };
        Catalog::parse(self, &value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn sample() -> (Binding, Value) {
        let accounts = json!({"revision":"r1","profiles":[{"id":"work","provider":"codex","home":"/accounts/example","auth_revision":"a1"}]});
        let binding = Binding::capture(
            crate::ComputerId::local(),
            "11111111-1111-4111-8111-111111111111".into(),
            &accounts,
            "work",
        )
        .unwrap();
        let reply = json!({"account_id":"work","provider":"codex","revision":"r1","effort_supported":true,"defaults":{"model":"missing","effort":"high"},"provider_defaults":{"model":"model-a","effort":"low"},"models":[{"id":"model-a","label":"Example model","effort_options":["low","high"]}],"private_unknown":"do not forward"});
        (binding, reply)
    }
    #[test]
    fn binds_account_provider_and_revision_and_keeps_a_closed_projection() {
        let (binding, reply) = sample();
        let parsed = Catalog::parse(&binding, &reply).unwrap();
        assert_eq!(parsed.defaults.model, "missing");
        assert_eq!(parsed.provider_defaults.effort, "low");
        assert!(!serde_json::to_string(&parsed)
            .unwrap()
            .contains("private_unknown"));
        for field in ["account_id", "provider", "revision"] {
            let mut changed = reply.clone();
            changed[field] = json!("other");
            assert!(Catalog::parse(&binding, &changed).is_err());
        }
    }
    #[test]
    fn unavailable_models_and_efforts_refuse_but_inheritance_is_allowed() {
        let (binding, mut reply) = sample();
        let parsed = Catalog::parse(&binding, &reply).unwrap();
        assert!(parsed.validate(&Values::default()).is_ok());
        assert!(parsed
            .validate(&Values {
                model: "missing".into(),
                ..Default::default()
            })
            .is_err());
        assert!(parsed
            .validate(&Values {
                model: "model-a".into(),
                effort: "high".into(),
                ..Default::default()
            })
            .is_ok());
        assert!(parsed
            .validate(&Values {
                model: "model-a".into(),
                effort: "max".into(),
                ..Default::default()
            })
            .is_err());
        reply["effort_supported"] = json!(false);
        let kimi = Catalog::parse(&binding, &reply).unwrap();
        assert!(kimi
            .validate(&Values {
                model: "model-a".into(),
                effort: "high".into(),
                ..Default::default()
            })
            .is_err());
    }
    #[test]
    fn choices_and_effort_lists_are_bounded() {
        let (binding, mut reply) = sample();
        reply["models"]=json!((0..300).map(|n|json!({"id":format!("model-{n}"),"effort_options":(0..100).map(|i|format!("level-{i}")).collect::<Vec<_>>()})).collect::<Vec<_>>());
        let parsed = Catalog::parse(&binding, &reply).unwrap();
        assert_eq!(parsed.models.len(), 256);
        assert_eq!(parsed.models[0].effort_options.len(), 64);
    }
}
