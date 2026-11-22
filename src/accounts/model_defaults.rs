//! Per-account launch choices. Native configuration and running sessions stay owned by the agent.
use super::*;
use std::io::Read;
use serde_json::value::RawValue;

/// Read entries leniently and keep each untouched JSON value verbatim. A
/// future shape for the whole field is also retained, but cannot be edited.
#[derive(Default)]
pub(super) struct Stored {
    entries: BTreeMap<String, Box<RawValue>>,
    opaque: Option<Box<RawValue>>,
}
impl Stored {
    pub(super) fn is_empty(&self) -> bool {
        self.opaque.is_none() && self.entries.is_empty()
    }
    fn editable(&mut self) -> Result<&mut BTreeMap<String, Box<RawValue>>> {
        if self.opaque.is_some() {
            return Err(Error::new(1, "This build cannot edit the saved model defaults format."));
        }
        Ok(&mut self.entries)
    }
}
impl Serialize for Stored {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match &self.opaque {
            Some(raw) => raw.serialize(serializer),
            None => self.entries.serialize(serializer),
        }
    }
}
impl<'de> Deserialize<'de> for Stored {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        Ok(match serde_json::from_str(raw.get()) {
            Ok(entries) => Self { entries, opaque: None },
            Err(_) => Self { entries: BTreeMap::new(), opaque: Some(raw) },
        })
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Defaults {
    #[serde(default)]
    model: String,
    #[serde(default)]
    effort: String,
}
fn supported(provider: &str) -> bool {
    ["codex", "claude", "kimi", "dsh"].contains(&provider)
}
/// DeepSeek's native reasoning efforts. Saving narrows them to the chosen
/// model's catalog entry; an unknown stored value fails closed.
const DSH_EFFORTS: [&str; 4] = ["off", "low", "high", "max"];
fn valid_effort(provider: &str, effort: &str) -> bool {
    effort.is_empty()
        || match provider {
            "codex" | "claude" => [
                "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
            ]
            .contains(&effort),
            "dsh" => DSH_EFFORTS.contains(&effort),
            _ => false,
        }
}
/// DeepSeek selects a native route as `provider/model`, with exactly one slash.
fn valid_model(provider: &str, model: &str) -> bool {
    safe(model)
        && (provider != "dsh"
            || model.split_once('/').is_some_and(|(route, id)| {
                !route.is_empty() && !id.is_empty() && !id.contains('/')
            }))
}
fn safe(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 120
        && !value.starts_with('-')
        && !value.chars().any(char::is_control)
}
fn provider_for<'a>(catalog: &'a Catalog, id: &'a str) -> Option<&'a str> {
    if native_id(id) {
        id.strip_prefix("native-")
    } else {
        catalog.profiles.get(id).map(|p| p.provider.as_str())
    }
}
fn validate(provider: &str, value: &Defaults) -> Result<()> {
    if !supported(provider)
        || !valid_model(provider, &value.model)
        || !valid_effort(provider, &value.effort)
    {
        return Err(Error::new(1, "Invalid account model defaults."));
    }
    Ok(())
}
struct Choice {
    defaults: Defaults,
    status: &'static str,
    reason: Option<&'static str>,
}
fn saved(catalog: &Catalog, id: &str) -> Choice {
    let inherited = || Choice { defaults: Defaults::default(), status: "inherited", reason: None };
    let ignored = |reason| Choice { defaults: Defaults::default(), status: "ignored", reason: Some(reason) };
    if catalog.model_defaults.opaque.is_some() {
        return ignored("This build does not support the saved model defaults format; it was kept unchanged.");
    }
    let Some(raw) = catalog.model_defaults.entries.get(id) else { return inherited(); };
    let provider = provider_for(catalog, id).unwrap_or("");
    if !supported(provider) {
        return ignored("This build does not support model defaults for this agent; the entry was kept unchanged.");
    }
    let Ok(defaults) = serde_json::from_str::<Defaults>(raw.get()) else {
        return ignored("This build does not support the saved model defaults fields; the entry was kept unchanged.");
    };
    if validate(provider, &defaults).is_err() {
        return ignored("This build cannot validate the saved model or effort; the entry was kept unchanged.");
    }
    Choice { defaults, status: "saved", reason: None }
}
fn project(row: &mut Value, catalog: &Catalog, id: &str, key: &str) {
    let choice = saved(catalog, id);
    row[key] = json!(choice.defaults);
    row["model_defaults_status"] = json!(choice.status);
    if let Some(reason) = choice.reason { row["model_defaults_reason"] = json!(reason); }
}
pub(super) fn describe(row: &mut Value, catalog: &Catalog) {
    let id = row["id"].as_str().unwrap_or("").to_owned();
    let provider = row["provider"].as_str().unwrap_or("");
    row["model_defaults_supported"] = json!(supported(provider));
    project(row, catalog, &id, "model_defaults");
}
fn native(provider: &str, home: &Path) -> Defaults {
    let mut text = String::new();
    let name = if provider == "claude" {
        "settings.json"
    } else {
        "config.toml"
    };
    if fs::File::open(home.join(name))
        .and_then(|f| f.take(1024 * 1024 + 1).read_to_string(&mut text))
        .is_err()
        || text.len() > 1024 * 1024
    {
        return Defaults::default();
    }
    if provider == "claude" {
        let data: Value = serde_json::from_str(&text).unwrap_or_default();
        return Defaults {
            model: data["model"]
                .as_str()
                .filter(|v| safe(v))
                .unwrap_or_default()
                .into(),
            effort: data["effortLevel"]
                .as_str()
                .filter(|v| safe(v))
                .unwrap_or_default()
                .into(),
        };
    }
    let Ok(data) = text.parse::<toml_edit::DocumentMut>() else {
        return Defaults::default();
    };
    let profile = data.get("profile").and_then(|v| v.as_str());
    let get = |key: &str| {
        profile
            .filter(|_| provider == "codex")
            .and_then(|p| data.get("profiles")?.get(p)?.get(key)?.as_str())
            .or_else(|| data.get(key).and_then(|v| v.as_str()))
            .filter(|v| safe(v))
            .unwrap_or_default()
            .to_owned()
    };
    Defaults {
        model: get(if provider == "kimi" {
            "default_model"
        } else {
            "model"
        }),
        effort: get(if provider == "kimi" {
            "default_reasoning_effort"
        } else {
            "model_reasoning_effort"
        }),
    }
}
pub(super) fn inspect(config: &Config, id: &str) -> Result<Value> {
    let bytes = read(config)?;
    let catalog = decode(&bytes)?;
    let (profile, home, _) = lookup(config, id)?;
    if !supported(&profile.provider) && profile.provider != "dsh" {
        return Err(Error::new(
            1,
            "This agent manages model defaults in its native settings.",
        ));
    }
    let mut result = models(&profile.provider, &home);
    // WhiteWh fork: a host that lists no models yet falls back to what the
    // account's own files show.
    if profile.provider == "dsh" && result["models"].as_array().is_some_and(|models| models.is_empty()) {
        let fallback = inspection::dsh(&home);
        if fallback["models"].as_array().is_some_and(|models| !models.is_empty()) {
            result["models"] = fallback["models"].clone();
            result["truncated"] = json!(false);
            result.as_object_mut().unwrap().remove("reason");
        }
    }
    // DeepSeek reports its native default (agent-default-model) with its catalog.
    let provider_defaults = if profile.provider == "dsh" {
        result["provider_defaults"].take()
    } else {
        json!(native(&profile.provider, &home))
    };
    result["account_id"] = json!(id);
    result["provider"] = json!(profile.provider);
    result["revision"] = json!(revision(&bytes));
    project(&mut result, &catalog, id, "defaults");
    result["provider_defaults"] = provider_defaults;
    result["effort_supported"] = json!(effort_supported(&profile.provider));
    Ok(result)
}
pub(super) fn set(config: &Config, args: &[String], dry: bool) -> Result<Value> {
    let id = args.get(1).map(String::as_str).unwrap_or("");
    let model = arg(args, "--model").ok_or_else(|| {
        Error::new(
            1,
            "Choose --model MODEL, or an empty value to inherit provider settings.",
        )
    })?;
    let effort = arg(args, "--effort").unwrap_or("");
    let _lock = locked(config)?;
    let bytes = read(config)?;
    check_revision(&bytes, args)?;
    let mut catalog = decode(&bytes)?;
    let (profile, home, native_account) = lookup(config, id)?;
    if !supported(&profile.provider) || catalog.hidden_profiles.contains(id) {
        return Err(Error::new(
            1,
            "Choose an available Codex, Claude, Kimi or DeepSeek account.",
        ));
    }
    if model.is_empty() {
        if !effort.is_empty() {
            return Err(Error::new(1, "Choose a model before setting effort."));
        }
        catalog.model_defaults.editable()?.remove(id);
    } else {
        let defaults = Defaults { model: model.into(), effort: effort.into() };
        validate(&profile.provider, &defaults)?;
        // Reject an uneditable catalog before starting an on-demand native refresh.
        let entries = catalog.model_defaults.editable()?;
        let mut models = models(&profile.provider, &home);
        if profile.provider == "codex" && !has_model(&models, model) {
            models = codex_catalog::refresh_missing(config, id, &profile, &home, native_account, model, dry)?;
        }
        let selected=selected_model(&models, model)
            .ok_or_else(||Error::new(1,if profile.provider=="dsh" {
                "The model is unavailable in the running DeepSeek host's catalog. Start or resume a DeepSeek session and refresh."
            } else {
                "The model is unavailable in this account's native catalog. Open the agent once and refresh."
            }))?;
        if !effort.is_empty()
            && (!effort_supported(&profile.provider)
                || !selected["effort_options"]
                    .as_array()
                    .is_some_and(|v| v.iter().any(|v| v == effort)))
        {
            return Err(Error::new(
                1,
                "Effort is unavailable for the selected model.",
            ));
        }
        let raw = serde_json::value::to_raw_value(&defaults).map_err(|e| Error::new(1, e.to_string()))?;
        entries.insert(id.into(), raw);
    }
    if !dry {
        save(config, &catalog)?;
    }
    list(config)
}
fn effort_supported(provider: &str) -> bool {
    ["codex", "claude", "dsh"].contains(&provider)
}
/// The account's model catalog. DeepSeek's comes from the resident host's
/// read-only catalog method; reading Accounts never starts a host.
pub(super) fn models(provider: &str, home: &Path) -> Value {
    if provider == "dsh" {
        crate::state::dsh_account_models(home)
    } else {
        crate::state::account_models(provider, home)
    }
}
pub(super) fn has_model(catalog: &Value, model: &str) -> bool {
    selected_model(catalog, model).is_some()
}
fn selected_model<'a>(catalog: &'a Value, model: &str) -> Option<&'a Value> {
    catalog["models"].as_array()?.iter().find(|entry| entry["id"] == model)
}
/// The saved DeepSeek launch selection as (provider/model, effort). The launch
/// applies it to a new session through the host's scoped settings method.
pub(super) fn launch_selection(config: &Config, provider: &str) -> Result<Option<(String, String)>> {
    if provider != "dsh" {
        return Ok(None);
    }
    let catalog = decode(&read(config)?)?;
    let choice = saved(&catalog, "native-dsh");
    Ok((choice.status == "saved").then_some((choice.defaults.model, choice.defaults.effort)))
}
pub(super) fn apply(
    config: &Config,
    account: &str,
    provider: &str,
    argv: &mut Vec<String>,
) -> Result<()> {
    let id = if account.is_empty() {
        format!("native-{provider}")
    } else {
        account.into()
    };
    let catalog = decode(&read(config)?)?;
    let choice = saved(&catalog, &id);
    if choice.status != "saved" {
        return Ok(());
    }
    if provider_for(&catalog, &id) != Some(provider) {
        return Err(Error::new(1, "The account belongs to another provider."));
    }
    apply_argv(provider, &choice.defaults, argv);
    Ok(())
}
fn apply_argv(provider: &str, defaults: &Defaults, argv: &mut Vec<String>) {
    let at = argv.iter().position(|v| v == "--").unwrap_or(argv.len());
    if at == 0 {
        return;
    }
    let args = &argv[1..at];
    let config_value = |i: usize, value: &str| -> Option<String> {
        let raw = if value == "-c" || value == "--config" {
            args.get(i + 1).map(String::as_str)
        } else if let Some(v) = value.strip_prefix("--config=") {
            Some(v)
        } else {
            value.strip_prefix("-c").filter(|v| !v.is_empty())
        };
        raw.and_then(|v| v.split_once('='))
            .map(|(key, _)| key.trim().to_owned())
    };
    let explicit_model = args.iter().any(|v| {
        ["--model", "-m", "--profile", "-P"].contains(&v.split('=').next().unwrap_or(""))
            || (v.starts_with("-m") && v.len() > 2)
            || (v.starts_with("-P") && v.len() > 2)
    }) || args
        .iter()
        .enumerate()
        .any(|(i, v)| config_value(i, v).as_deref() == Some("model"));
    if explicit_model {
        return;
    }
    let explicit_effort = args.iter().enumerate().any(|(i, v)| {
        v.split('=').next() == Some("--effort")
            || config_value(i, v).as_deref() == Some("model_reasoning_effort")
    });
    let mut values = vec!["--model".into(), defaults.model.clone()];
    if !defaults.effort.is_empty() && !explicit_effort {
        if provider == "codex" {
            values.extend([
                "-c".into(),
                format!("model_reasoning_effort=\"{}\"", defaults.effort),
            ]);
        } else if provider == "claude" {
            values.extend(["--effort".into(), defaults.effort.clone()]);
        }
    }
    argv.splice(at..at, values);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deepseek_defaults_are_one_native_route_and_a_native_effort() {
        let accepted = |id: &str, model: &str, effort: &str| {
            let defaults = Defaults { model: model.into(), effort: effort.into() };
            let bytes = serde_json::to_vec(&json!({"model_defaults":{(id):&defaults}})).unwrap();
            let catalog = decode(&bytes).unwrap();
            let allowed = validate(provider_for(&catalog, id).unwrap_or(""), &defaults).is_ok();
            let choice = saved(&catalog, id);
            assert_eq!(choice.status, if allowed { "saved" } else { "ignored" });
            if allowed {
                assert_eq!((choice.defaults.model.as_str(), choice.defaults.effort.as_str()), (model, effort));
            }
            allowed
        };
        let route = "deepseek-official/deepseek-flash";
        assert!(accepted("native-dsh", route, ""));
        for effort in DSH_EFFORTS {
            assert!(accepted("native-dsh", route, effort));
        }
        for model in ["deepseek-flash", "/deepseek-flash", "deepseek-official/", "a/b/c", "-a/b", ""] {
            assert!(!accepted("native-dsh", model, "max"), "{model}");
        }
        for effort in ["medium", "xhigh", "ultra", "none", "MAX", " max"] {
            assert!(!accepted("native-dsh", route, effort), "{effort}");
        }
        // Other providers keep their own effort vocabularies.
        assert!(accepted("native-codex", "fixture-model", "high"));
        assert!(!accepted("native-codex", "fixture-model", "off"));
        assert!(!accepted("native-kimi", "fixture-model", "high"));
        assert!(!accepted("native-opencode", route, ""));
        assert!(effort_supported("dsh") && supported("dsh"));
    }
    #[test]
    fn reads_ignore_unusable_entries_and_round_trip_their_original_json() {
        let future = r#"{ "route": ["future", 1.00, 1e+02], "effort": {"level":"new"} }"#;
        let newer = r#"{ "model": "fixture-model", "effort": "future-effort" }"#;
        let raw = format!(r#"{{"model_defaults":{{"native-future":{future},"native-kimi":{newer},
            "native-codex":{{"model":"fixture-code","effort":"high"}},
            "native-claude":{{"model":"fixture-claude","effort":"max"}}}}}}"#);
        let catalog = decode(raw.as_bytes()).unwrap();
        for id in ["native-future", "native-kimi"] {
            let choice = saved(&catalog, id);
            assert_eq!(choice.status, "ignored");
            assert!(choice.reason.is_some());
            assert!(choice.defaults.model.is_empty());
        }
        for (id, provider, model) in [("native-codex", "codex", "fixture-code"), ("native-claude", "claude", "fixture-claude")] {
            let choice = saved(&catalog, id);
            assert_eq!(choice.status, "saved");
            let mut argv = vec![provider.into()];
            apply_argv(provider, &choice.defaults, &mut argv);
            assert!(argv.contains(&model.to_owned()));
        }
        let output = serde_json::to_string_pretty(&catalog).unwrap();
        assert!(output.contains(future));
        assert!(output.contains(newer));
    }

    #[test]
    fn every_entry_shape_is_lenient_on_read_but_known_writes_are_strict() {
        for value in [r#"{"model":"fixture","extra":true}"#, r#"{"model":42}"#,
            r#"{"model":"fixture","effort":"future"}"#, r#"{"model":"-unsafe"}"#,
            r#"{"model":"fixture","effort":null}"#, "null", "[]", "false", "42", r#""future-format""#] {
            let raw = format!(r#"{{"model_defaults":{{"native-codex":{value}}}}}"#);
            let catalog = decode(raw.as_bytes()).unwrap();
            assert_eq!(saved(&catalog, "native-codex").status, "ignored", "{value}");
            assert!(serde_json::to_string(&catalog).unwrap().contains(value));
        }
        for value in ["null", "[]", "42", r#""future-format""#] {
            let raw = format!(r#"{{"model_defaults":{value}}}"#);
            let mut catalog = decode(raw.as_bytes()).unwrap();
            assert_eq!(saved(&catalog, "native-codex").status, "ignored");
            assert!(catalog.model_defaults.editable().is_err());
            assert!(serde_json::to_string(&catalog).unwrap().contains(value));
        }
        for (provider, model, effort) in [("codex", "-unsafe", ""), ("codex", "fixture", "future"),
            ("kimi", "fixture", "high"), ("unknown", "fixture", ""), ("claude", "", "")] {
            assert!(validate(provider, &Defaults { model: model.into(), effort: effort.into() }).is_err());
        }
        assert!(decode(br#"{"model_defaults":{"native-codex":broken}}"#).is_err());
    }

    #[test]
    fn native_choices_and_prompt_text_keep_their_meaning() {
        let defaults = Defaults {
            model: "fixture-model".into(),
            effort: "high".into(),
        };
        for provider in ["codex", "claude"] {
            let mut argv = vec![
                provider.into(),
                "--".into(),
                "--model".into(),
                "prompt-only".into(),
            ];
            apply_argv(provider, &defaults, &mut argv);
            assert_eq!(&argv[1..3], ["--model", "fixture-model"]);
            assert_eq!(&argv[argv.len() - 3..], ["--", "--model", "prompt-only"]);
            for explicit in [
                vec!["--model", "explicit"],
                vec!["-m=explicit"],
                vec!["-c", "model = 'explicit'"],
                vec!["--profile", "explicit"],
            ] {
                let mut argv = vec![provider.into()];
                argv.extend(explicit.iter().map(|v| v.to_string()));
                let before = argv.clone();
                apply_argv(provider, &defaults, &mut argv);
                assert_eq!(argv, before);
            }
        }
        let mut argv = vec![
            "codex".into(),
            "-c".into(),
            "model_reasoning_effort=low".into(),
        ];
        apply_argv("codex", &defaults, &mut argv);
        assert!(argv.contains(&"fixture-model".into()));
        assert_eq!(
            argv.iter()
                .filter(|v| v.contains("model_reasoning_effort"))
                .count(),
            1
        );
    }
}
