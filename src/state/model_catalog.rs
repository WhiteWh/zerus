//! Owner spec 2026-11-14 (`zerus-model-catalog-tz.md`): model servers on the
//! `model-catalog/2` contract.
//!
//! The catalog is one JSON document, read through the SSH transport (an argv
//! that prints it) or the HTTP API (`GET /v1/catalog` with a private key). Each
//! service carries its state, variants, live context window and the gateway's
//! API names with their wire format, so a session takes its API name and
//! context from the catalog instead of static `contexts`, and one server row
//! serves Claude Code (anthropic wire) and Codex (openai wire) alike. The last
//! catalog read is cached privately, so a launch never waits more than a few
//! seconds and a status read of an unreachable server still shows it, marked
//! with its age.
use super::model_servers::{self as servers, CatalogApi, Failure, Server, CATALOG_PROTOCOL};
use crate::{
    cli::{Error, Result},
    config::Config,
    platform,
    state::storage,
};
use serde_json::{json, Value};
use std::{path::PathBuf, time::Duration};

const CATALOG_SECS: u64 = 20;
/// A launch waits this long for a fresh catalog, then uses the cached one.
const LAUNCH_SECS: u64 = 5;
const ACTION_SECS: u64 = 120;
const MAX_SERVICES: usize = 500;
/// Fields a status row carries over from the catalog as they are.
const CARRIED: &[&str] = &[
    "display_name", "type", "managed", "state_detail", "exclusive", "memory_gib", "needs_gib", "fit",
    "context", "context_max", "variant", "variant_selected", "variants", "api_models", "use", "use_for", "actions",
];

pub(crate) fn is_catalog(server: &Server) -> bool {
    server.protocol == CATALOG_PROTOCOL
}

/// The catalog API as a plain server row: its key goes as a Bearer token.
fn api_server(api: &CatalogApi) -> Server {
    Server { name: "catalog".into(), base_url: api.base_url.clone(), kind: "openai".into(), key: api.key.clone(), ..Default::default() }
}

/// One call of the catalog API. 401/403 is unauthorized, 502 (the model's own
/// server is down) unreachable, any other failure refused with the server's
/// `error` text.
fn api_call(config: &Config, api: &CatalogApi, method: &str, path: &str, body: Option<&Value>, timeout: Duration) -> std::result::Result<Value, Failure> {
    let url = format!("{}{path}", api.base_url);
    let server = api_server(api);
    let body = body.map(Value::to_string);
    let (code, text) = if url.starts_with("https://") {
        servers::curl_send(config, &server, &url, body.as_deref())?
    } else {
        servers::http_send(method, &url, &server, timeout, body.as_deref())?
    };
    if !(200..300).contains(&code) {
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|value| value["error"].as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("The catalog API answered HTTP {code}"));
        return Err(match code {
            401 | 403 => Failure::Unauthorized(format!("The catalog API refused its key (HTTP {code})")),
            502 => Failure::Unreachable(detail),
            _ => Failure::Refused(detail),
        });
    }
    serde_json::from_str(&text).map_err(|_| Failure::Refused("The catalog API did not answer JSON".into()))
}

fn cache_path(config: &Config, server: &Server) -> PathBuf {
    config.state.join("model-servers").join(format!("catalog-{}.json", server.name))
}

fn write_cache(config: &Config, server: &Server, catalog: &Value) {
    let dir = config.state.join("model-servers");
    if platform::private_dir(&dir).is_ok() {
        let text = json!({"fetched_at": super::now(), "catalog": catalog}).to_string();
        let _ = storage::atomic_private(&cache_path(config, server), &text);
    }
}

/// The last catalog read successfully, with when it was read.
pub(crate) fn cached(config: &Config, server: &Server) -> Option<(f64, Value)> {
    let value: Value = serde_json::from_slice(&std::fs::read(cache_path(config, server)).ok()?).ok()?;
    let at = value["fetched_at"].as_f64()?;
    let catalog = value["catalog"].clone();
    valid(&catalog).then_some((at, catalog))
}

fn valid(catalog: &Value) -> bool {
    catalog["protocol"].as_str() == Some(CATALOG_PROTOCOL) && catalog["models"].is_array()
}

/// Reads the catalog now, through the API or the catalog command, and caches it.
pub(crate) fn fetch(config: &Config, server: &Server, timeout: Duration) -> std::result::Result<Value, Failure> {
    let catalog = if let Some(api) = &server.api {
        api_call(config, api, "GET", "/v1/catalog", None, timeout)?
    } else if !server.catalog.is_empty() {
        let argv = servers::filled(&server.catalog, "", &server.name);
        let run = servers::run_template(config, server, &argv, timeout)?;
        serde_json::from_str(run.stdout.trim()).map_err(|_| Failure::Refused("The catalog command did not print JSON".into()))?
    } else {
        return Err(Failure::Refused(format!("{} has neither a catalog command nor an api", server.name)));
    };
    if !valid(&catalog) {
        return Err(Failure::Refused(format!("The catalog is not {CATALOG_PROTOCOL}")));
    }
    write_cache(config, server, &catalog);
    Ok(catalog)
}

/// The catalog's state words as the tab shows them; a Russian word from an
/// older tool becomes its English state.
fn state_word(text: &str) -> String {
    let lower = text.trim().to_lowercase();
    match lower.as_str() {
        "готова" | "работает" => "ready".into(),
        "выключена" | "остановлена" => "stopped".into(),
        "запускается" => "starting".into(),
        "останавливается" => "stopping".into(),
        _ if lower.is_empty() => "unknown".into(),
        _ => lower,
    }
}

fn services(catalog: &Value) -> impl Iterator<Item = &Value> {
    catalog["models"]
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_SERVICES)
        .filter(|model| model["name"].as_str().is_some_and(|name| !name.is_empty() && !name.starts_with('-')))
}

fn api_ids(service: &Value) -> impl Iterator<Item = (&str, &str)> {
    service["api_models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| Some((entry["id"].as_str()?, entry["wire"].as_str().unwrap_or_default())))
}

/// The service a model name means: its own name or one of its API names.
pub(crate) fn service_of<'a>(catalog: &'a Value, model: &str) -> Option<&'a Value> {
    services(catalog).find(|service| service["name"].as_str() == Some(model) || api_ids(service).any(|(id, _)| id == model))
}

fn selected_variant(service: &Value) -> Option<&Value> {
    let selected = service["variant"].as_str().or_else(|| service["variant_selected"].as_str());
    let variants = service["variants"].as_array()?;
    variants
        .iter()
        .find(|variant| variant["running"] == true)
        .or_else(|| variants.iter().find(|variant| variant["selected"] == true))
        .or_else(|| variants.iter().find(|variant| variant["id"].as_str() == selected))
}

/// A manual window in `contexts` overrides the catalog for that name.
fn manual_context(server: &Server, service: &Value) -> Option<u64> {
    let name = service["name"].as_str().unwrap_or_default();
    server.contexts.get(name).copied().or_else(|| api_ids(service).find_map(|(id, _)| server.contexts.get(id).copied()))
}

/// The status rows of a catalog: the v1 fields (name, state, since, memory)
/// plus what the catalog says of each service.
fn status_value(server: &Server, catalog: &Value, fetched_at: f64) -> Value {
    let models: Vec<Value> = services(catalog)
        .map(|service| {
            let mut row = json!({"name": service["name"], "state": state_word(service["state"].as_str().unwrap_or_default())});
            if let Some(since) = service["since"].as_f64().filter(|since| *since > 0.0) {
                row["since"] = json!(since);
            }
            for field in CARRIED {
                if !service[*field].is_null() {
                    row[*field] = service[*field].clone();
                }
            }
            if let Some(variant) = selected_variant(service) {
                row["speed"] = json!({"tok_s": variant["tok_s"], "code_tok_s": variant["code_tok_s"]});
            }
            if let Some(tokens) = manual_context(server, service) {
                row["context_manual"] = json!(tokens);
            }
            row
        })
        .collect();
    let memory = &catalog["memory"];
    let memory = match (memory["free"].as_f64(), memory["total"].as_f64()) {
        (Some(free), Some(total)) => json!({"free": free, "total": total, "unit": memory["unit"].as_str().unwrap_or("GiB")}),
        _ => Value::Null,
    };
    json!({"server": server.name, "source": "catalog", "protocol": CATALOG_PROTOCOL, "models": models, "memory": memory,
        "gateway": catalog["gateway"], "fetched_at": fetched_at, "generated_at": catalog["generated_at"], "raw": []})
}

/// `status` of a catalog server. When the server does not answer, the last
/// cached catalog is shown, marked stale with its time and the failure; with
/// no cache the failure itself is the answer.
pub(crate) fn status(config: &Config, server: &Server) -> std::result::Result<Value, Failure> {
    match fetch(config, server, Duration::from_secs(CATALOG_SECS)) {
        Ok(catalog) => Ok(status_value(server, &catalog, super::now())),
        Err(failure) => {
            let Some((at, catalog)) = cached(config, server) else { return Err(failure) };
            let mut value = status_value(server, &catalog, at);
            let error = failure.value();
            value["stale"] = json!(true);
            value["error"] = error["error"].clone();
            value["detail"] = error["detail"].clone();
            Ok(value)
        }
    }
}

/// The catalog a launch uses: a fresh read within LAUNCH_SECS, else the cache.
fn for_launch(config: &Config, server: &Server) -> Option<(Value, Option<f64>)> {
    match fetch(config, server, Duration::from_secs(LAUNCH_SECS)) {
        Ok(catalog) => Some((catalog, None)),
        Err(_) => cached(config, server).map(|(at, catalog)| (catalog, Some(at))),
    }
}

/// What a session on a catalog service gets: the API name of its agent's
/// wire, that wire's base URL and the service's context window.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Resolved {
    pub service: String,
    pub api_model: String,
    pub base_url: String,
    pub context: Option<u64>,
    pub manual_context: bool,
    pub variant: String,
    /// The cached catalog's time when the server did not answer in time.
    pub stale_at: Option<f64>,
}

pub(crate) fn wire_for(agent: &str) -> Option<&'static str> {
    match agent {
        "claude" => Some("anthropic"),
        "codex" => Some("openai"),
        _ => None,
    }
}

/// Resolves `model` (a service or one of its API names) for `agent`. None when
/// no catalog was ever read: the launch then follows the row's own fields.
pub(crate) fn resolve(config: &Config, server: &Server, agent: &str, model: &str) -> Result<Option<Resolved>> {
    let Some((catalog, stale_at)) = for_launch(config, server) else { return Ok(None) };
    resolve_in(server, &catalog, agent, model, stale_at).map(Some)
}

pub(crate) fn resolve_in(server: &Server, catalog: &Value, agent: &str, model: &str, stale_at: Option<f64>) -> Result<Resolved> {
    let wire = wire_for(agent).ok_or_else(|| Error::new(1, format!("A model server session starts claude or codex, not {agent}")))?;
    let service = service_of(catalog, model)
        .ok_or_else(|| Error::new(1, format!("{model} is not in the catalog of model server {}", server.name)))?;
    let name = service["name"].as_str().unwrap_or_default().to_owned();
    let api_model = api_ids(service)
        .find(|(id, kind)| *id == model && *kind == wire)
        .or_else(|| api_ids(service).find(|(_, kind)| *kind == wire))
        .map(|(id, _)| id.to_owned())
        .ok_or_else(|| {
            Error::new(1, format!(
                "{name} on {} has no {wire} API name, so {} cannot use it",
                server.name,
                if agent == "claude" { "Claude Code" } else { "Codex" }
            ))
        })?;
    servers::checked_model(&api_model)?;
    let gateway = &catalog["gateway"];
    let base_url = gateway[if wire == "anthropic" { "anthropic_base_url" } else { "openai_base_url" }]
        .as_str()
        .filter(|url| servers::checked_base_url(url))
        .map(|url| url.trim_end_matches('/').to_owned())
        .unwrap_or_else(|| if wire == "anthropic" { servers::anthropic_base(server) } else { servers::openai_base(server) });
    let manual = manual_context(server, service);
    let live = service["context"].as_u64().filter(|tokens| (4096..=10_000_000).contains(tokens));
    Ok(Resolved {
        service: name,
        api_model,
        base_url,
        context: manual.or(live),
        manual_context: manual.is_some(),
        variant: service["variant"].as_str().or_else(|| service["variant_selected"].as_str()).unwrap_or_default().to_owned(),
        stale_at,
    })
}

/// The API names a Claude Code `/model` picker may offer: anthropic wire only
/// (an OpenAI name goes through format translation and breaks the agent),
/// each with its service's window.
pub(crate) fn picker_names(config: &Config, server: &Server) -> Option<Vec<(String, Option<u64>)>> {
    let (catalog, _) = for_launch(config, server)?;
    Some(
        services(&catalog)
            .flat_map(|service| {
                let context = manual_context(server, service).or_else(|| service["context"].as_u64());
                api_ids(service)
                    .filter(|(_, wire)| *wire == "anthropic")
                    .map(move |(id, _)| (id.to_owned(), context))
                    .collect::<Vec<_>>()
            })
            .collect(),
    )
}

/// Normalized root of a base URL, for comparing an account's gateway with the
/// catalog's.
fn url_root(url: &str) -> String {
    url.trim_end_matches('/').trim_end_matches("/v1").trim_end_matches('/').to_lowercase()
}

/// The service a live session uses on `server`, if any: a session started on
/// the server (by its recorded service, else by its API name), or a session of
/// an account whose own gateway is this catalog's gateway.
pub(crate) fn session_service(server: &Server, catalog: &Value, record: &Value) -> Option<(String, String)> {
    let binding = &record["model_server"];
    if binding["server"].as_str() == Some(server.name.as_str()) {
        let model = binding["api_model"].as_str().or_else(|| binding["model"].as_str()).unwrap_or_default();
        let service = binding["service"]
            .as_str()
            .filter(|service| !service.is_empty())
            .map(str::to_owned)
            .or_else(|| service_of(catalog, model).and_then(|service| service["name"].as_str().map(str::to_owned)))
            .unwrap_or_default();
        return Some((service, model.to_owned()));
    }
    if binding.is_object() {
        return None;
    }
    let home = record["agent_home"].as_str().filter(|home| !home.is_empty())?;
    let settings: Value = serde_json::from_slice(&std::fs::read(std::path::Path::new(home).join("settings.json")).ok()?).ok()?;
    let base = settings["env"]["ANTHROPIC_BASE_URL"].as_str()?;
    let gateway = catalog["gateway"]["anthropic_base_url"].as_str()?;
    if url_root(base) != url_root(gateway) {
        return None;
    }
    let model = settings["env"]["ANTHROPIC_MODEL"].as_str().unwrap_or_default();
    let service = service_of(catalog, model)?;
    Some((service["name"].as_str().unwrap_or_default().to_owned(), model.to_owned()))
}

/// `sessions SERVER --json` of a catalog server: every live session using one
/// of its services, by its own binding or its account's gateway.
pub(crate) fn sessions_using(server: &Server, catalog: &Value, records: impl Iterator<Item = (String, Value)>) -> Vec<Value> {
    records
        .filter_map(|(name, record)| {
            let (service, model) = session_service(server, catalog, &record)?;
            let via = if record["model_server"].is_object() { "server" } else { "account" };
            Some(json!({"name": name, "model": model, "service": service, "via": via}))
        })
        .collect()
}

/// The sessions of `service`, for a Down or variant confirmation.
fn service_sessions(config: &Config, server: &Server, catalog: &Value, service: &str) -> Vec<Value> {
    let _ = config;
    servers::live_records()
        .map(|records| sessions_using(server, catalog, records.into_iter()))
        .unwrap_or_default()
        .into_iter()
        .filter(|session| session["service"].as_str() == Some(service))
        .collect()
}

fn running(service: &Value) -> bool {
    matches!(state_word(service["state"].as_str().unwrap_or_default()).as_str(), "ready" | "starting" | "running")
}

/// `hgs model-server variant SERVER MODEL VARIANT [--restart] [--yes] [--detach]`.
/// The variant is checked against the catalog before anything runs. A stopped
/// service changes its selection at once; a running one restarts, which needs
/// `--restart` and a confirmation naming the sessions it interrupts.
pub(crate) fn variant(config: &Config, server: &Server, model: &str, variant: &str, restart: bool, yes: bool, detach: bool) -> std::result::Result<Value, Failure> {
    let model = servers::checked_model(model).map_err(|error| Failure::Refused(error.message))?;
    let variant = servers::checked_model(variant).map_err(|_| Failure::Refused("A variant uses letters, digits and -_.:/@+".into()))?;
    if !is_catalog(server) {
        return Err(Failure::Refused(format!("{} does not use {CATALOG_PROTOCOL}", server.name)));
    }
    if server.api.is_none() && server.variant.is_empty() {
        return Err(Failure::Refused(format!("{} has no variant command", server.name)));
    }
    let catalog = fetch(config, server, Duration::from_secs(CATALOG_SECS))?;
    let service = services(&catalog)
        .find(|service| service["name"].as_str() == Some(model.as_str()))
        .ok_or_else(|| Failure::Refused(format!("{model} is not a service of {}", server.name)))?;
    if service["managed"] == false {
        return Err(Failure::Refused(format!("{model} runs outside the server's control; its variant cannot change here")));
    }
    if service["actions"]["variant"] == false {
        return Err(Failure::Refused(format!("{model} offers no variant change")));
    }
    let known = service["variants"].as_array().into_iter().flatten().any(|entry| entry["id"].as_str() == Some(variant.as_str()));
    if variant != "auto" && !known {
        return Err(Failure::Refused(format!("{model} has no variant {variant}")));
    }
    let live = running(service);
    if live && !restart {
        return Err(Failure::Refused(format!("{model} is running: changing its variant restarts it; pass --restart")));
    }
    if live && !yes {
        return Ok(json!({"status": "confirm", "server": server.name, "model": model, "variant": variant, "restart": true,
            "sessions": service_sessions(config, server, &catalog, &model),
            "note": format!("Changing {model} to {variant} restarts it on {}; its sessions lose their answers until it is ready. Pass --yes to run it.", server.name)}));
    }
    if let Some(api) = &server.api {
        let path = format!("/v1/models/{model}/engine");
        let answer = api_call(config, api, "POST", &path, Some(&json!({"engine": variant, "restart": live, "wait": false})), Duration::from_secs(ACTION_SECS))?;
        if answer["ok"] == false {
            return Err(Failure::Refused(answer["message"].as_str().unwrap_or("The server refused the variant").to_owned()));
        }
        return Ok(json!({"status": "ran", "server": server.name, "model": model, "variant": variant,
            "restarted": answer["restarted"].as_bool().unwrap_or(live), "message": answer["message"]}));
    }
    // The command door changes only a stopped service: a running one goes
    // down, changes, and comes up again.
    if live {
        servers::down(config, server, &model, true)?;
    }
    let argv: Vec<String> = servers::filled(&server.variant, &model, &server.name)
        .into_iter()
        .map(|arg| arg.replace("{variant}", &variant))
        .collect();
    let run = servers::run_template(config, server, &argv, Duration::from_secs(ACTION_SECS))?;
    let mut receipt = json!({"status": "ran", "server": server.name, "model": model, "variant": variant, "restarted": live, "exit": run.code});
    if live {
        let up = servers::up(config, server, &model, detach)?;
        receipt["up"] = up;
    }
    Ok(receipt)
}

/// Up, Down and Logs through the catalog API when the server has no command
/// for them.
pub(crate) fn api_up(config: &Config, server: &Server, api: &CatalogApi, model: &str) -> std::result::Result<Value, Failure> {
    let answer = api_call(config, api, "POST", &format!("/v1/models/{model}/start"), Some(&json!({"wait": false})), Duration::from_secs(ACTION_SECS))?;
    if answer["ok"] == false {
        return Err(Failure::Refused(answer["message"].as_str().unwrap_or("The server refused to start the model").to_owned()));
    }
    Ok(json!({"status": "ran", "server": server.name, "model": model, "message": answer["message"]}))
}

pub(crate) fn api_down(config: &Config, server: &Server, api: &CatalogApi, model: &str) -> std::result::Result<Value, Failure> {
    let answer = api_call(config, api, "POST", &format!("/v1/models/{model}/stop"), Some(&json!({})), Duration::from_secs(ACTION_SECS))?;
    if answer["ok"] == false {
        return Err(Failure::Refused(answer["message"].as_str().unwrap_or("The server refused to stop the model").to_owned()));
    }
    Ok(json!({"status": "ran", "server": server.name, "model": model, "message": answer["message"]}))
}

pub(crate) fn api_logs(config: &Config, api: &CatalogApi, model: &str, lines: usize) -> std::result::Result<String, Failure> {
    let answer = api_call(config, api, "GET", &format!("/v1/models/{model}/logs?lines={lines}"), None, Duration::from_secs(CATALOG_SECS))?;
    Ok(match &answer {
        Value::String(text) => text.clone(),
        _ => answer["logs"].as_str().or_else(|| answer["text"].as_str()).map(str::to_owned).unwrap_or_else(|| {
            answer["lines"].as_array().map(|lines| lines.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n")).unwrap_or_default()
        }),
    })
}

/// The sessions a Down of `model` interrupts, for its confirmation.
pub(crate) fn down_sessions(config: &Config, server: &Server, model: &str) -> Vec<Value> {
    let Some((_, catalog)) = cached(config, server) else { return Vec::new() };
    let Some(service) = service_of(&catalog, model).and_then(|service| service["name"].as_str()) else { return Vec::new() };
    service_sessions(config, server, &catalog, service)
}

#[cfg(test)]
#[path = "model_catalog_tests.rs"]
mod tests;
