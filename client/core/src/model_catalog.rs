//! Closed, bounded projection of an account's native model catalog.
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

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
    pub effort_labels: Vec<String>,
    pub default_effort: String,
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

pub fn defaults(value: &Value) -> Defaults {
    Defaults {
        model: short(&value["model"], 256),
        effort: short(&value["effort"], 40),
    }
}

pub fn parse_models(value: &Value) -> Result<Vec<Model>, String> {
    let rows = value["models"].as_array().ok_or(INVALID)?;
    let mut seen = BTreeSet::new();
    let mut models = Vec::new();
    for row in rows.iter().take(256) {
        let id = short(&row["id"], 256);
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        let label = short(&row["label"], 160);
        let labels = row["effort_labels"].as_array();
        let mut effort_options = Vec::new();
        let mut effort_labels = Vec::new();
        for (index, value) in row["effort_options"]
            .as_array()
            .into_iter()
            .flatten()
            .take(64)
            .enumerate()
        {
            let effort = short(value, 40);
            if effort.is_empty() || effort_options.contains(&effort) {
                continue;
            }
            let effort_label = labels
                .and_then(|values| values.get(index))
                .map(|value| short(value, 80))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| effort.clone());
            effort_options.push(effort.clone());
            effort_labels.push(effort_label);
        }
        let default_effort = short(&row["default_effort"], 40);
        models.push(Model {
            id: id.clone(),
            label: if label.is_empty() { id } else { label },
            default_effort: effort_options
                .contains(&default_effort)
                .then_some(default_effort)
                .unwrap_or_default(),
            effort_options,
            effort_labels,
        });
    }
    Ok(models)
}

impl Catalog {
    pub fn from_value(value: &Value) -> Result<Self, String> {
        Ok(Self {
            models: parse_models(value)?,
            defaults: defaults(&value["defaults"]),
            provider_defaults: defaults(&value["provider_defaults"]),
            effort_supported: value["effort_supported"] == true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn models_keep_aligned_effort_labels_and_a_reported_default() {
        let value = json!({"models":[{
            "id":"model-a","label":"Example","effort_options":["low","high","low"],
            "effort_labels":["Low","High"],"default_effort":"high"
        },{"id":"model-b","effort_options":["low"],"default_effort":"max"}]});
        let models = parse_models(&value).unwrap();
        assert_eq!(models[0].effort_options, ["low", "high"]);
        assert_eq!(models[0].effort_labels, ["Low", "High"]);
        assert_eq!(models[0].default_effort, "high");
        assert_eq!(models[1].effort_labels, ["low"]);
        assert_eq!(models[1].default_effort, "");
    }
}
