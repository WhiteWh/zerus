//! Owner spec 2026-11-14: model servers on `model-catalog/2`.
use super::*;
use crate::state::model_servers::{checked, public, session_settings, set_fake_command, Run};
use std::{cell::RefCell, rc::Rc};

fn config(dir: &std::path::Path) -> Config {
    Config {
        home: dir.display().to_string(),
        dir: dir.to_path_buf(),
        state: dir.join("state"),
        selves: Vec::new(),
        peers: Vec::new(),
        base_peers: Vec::new(),
        peers_from_environment: false,
        machines: std::collections::BTreeMap::new(),
        tab: false,
        tab_colors: String::new(),
    }
}

fn server() -> Server {
    checked(Server {
        name: "gb10".into(),
        base_url: "http://192.0.2.79:4000".into(),
        kind: "anthropic".into(),
        ssh: "gb10".into(),
        protocol: CATALOG_PROTOCOL.into(),
        catalog: vec!["gb10-model".into(), "catalog".into()],
        variant: vec!["gb10-model".into(), "engine".into(), "{model}".into(), "{variant}".into()],
        up: vec!["gb10-model".into(), "up".into(), "{model}".into()],
        down: vec!["gb10-model".into(), "down".into(), "{model}".into()],
        ..Default::default()
    })
    .unwrap()
}

/// The GB10 catalog of the spec, with the running variant and state given.
fn catalog(variant: &str, state: &str) -> Value {
    let context = if variant.ends_with("-1m") { 1_000_000 } else { 262_144 };
    let variants: Vec<Value> = [("llama", 262_144), ("strata", 262_144), ("llama-1m", 1_000_000), ("strata-1m", 1_000_000)]
        .iter()
        .map(|(id, window)| json!({"id": id, "context": window, "needs_gib": 100, "tok_s": 40.0, "code_tok_s": 60.0,
            "selected": *id == variant, "running": *id == variant && state == "ready"}))
        .collect();
    json!({
        "protocol": "model-catalog/2", "generated_at": 1794621120, "host": "gx10.example",
        "memory": {"free": 14.2, "total": 121.6, "unit": "GiB"},
        "gateway": {"openai_base_url": "http://192.0.2.79:4000/v1", "anthropic_base_url": "http://192.0.2.79:4000", "up": true},
        "models": [{
            "name": "qwen3.8-flash-next", "type": "llm", "managed": true, "state": state, "since": 1794615847,
            "memory_gib": 58.6, "needs_gib": 100.0, "fit": null, "context": context, "context_max": 1_000_000,
            "variant": variant, "variant_selected": variant, "variants": variants,
            "api_models": [
                {"id": "qwen3.8-flash-next", "wire": "openai", "for": ["codex", "openai"]},
                {"id": "qwen3.8-flash-next-coder", "wire": "anthropic", "for": ["claude-code"]}],
            "actions": {"up": state != "ready", "down": state == "ready", "variant": true, "logs": true}
        }, {
            "name": "runtime:comfyui-h3img", "type": "comfyui", "managed": false, "state": "external",
            "variants": [], "api_models": [], "actions": {"up": false, "down": false, "variant": false, "logs": false}
        }]
    })
}

/// Every command the fake transport ran, and a catalog it prints.
fn fake(document: Rc<RefCell<Value>>) -> Rc<RefCell<Vec<Vec<String>>>> {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&calls);
    set_fake_command(Some(Box::new(move |_: &Server, argv: &[String], _| {
        seen.borrow_mut().push(argv.to_vec());
        let stdout = if argv.get(1).map(String::as_str) == Some("catalog") { document.borrow().to_string() } else { String::new() };
        Ok(Run { code: Some(0), stdout, stderr: String::new() })
    })));
    calls
}

#[test]
fn a_catalog_status_carries_variants_live_context_and_api_names() {
    let temp = tempfile::tempdir().unwrap();
    let config = config(temp.path());
    let _calls = fake(Rc::new(RefCell::new(catalog("strata-1m", "ready"))));
    let value = servers::status(&config, &server()).unwrap();
    set_fake_command(None);
    assert_eq!(value["source"], "catalog");
    assert_eq!(value["memory"], json!({"free": 14.2, "total": 121.6, "unit": "GiB"}));
    let qwen = &value["models"][0];
    assert_eq!((qwen["name"].as_str(), qwen["state"].as_str()), (Some("qwen3.8-flash-next"), Some("ready")));
    assert_eq!(qwen["since"], json!(1794615847.0));
    assert_eq!(qwen["context"], 1_000_000);
    assert_eq!(qwen["variant"], "strata-1m");
    assert_eq!(qwen["variants"].as_array().unwrap().len(), 4);
    assert_eq!(qwen["api_models"][1]["wire"], "anthropic");
    assert_eq!(qwen["speed"], json!({"tok_s": 40.0, "code_tok_s": 60.0}));
    let comfy = &value["models"][1];
    assert_eq!((comfy["managed"].as_bool(), comfy["state"].as_str()), (Some(false), Some("external")));
}

#[test]
fn an_unreachable_server_shows_its_cached_catalog_marked_stale() {
    let temp = tempfile::tempdir().unwrap();
    let config = config(temp.path());
    let _calls = fake(Rc::new(RefCell::new(catalog("strata", "stopped"))));
    servers::status(&config, &server()).unwrap();
    set_fake_command(Some(Box::new(|_: &Server, _: &[String], _| Err(Failure::Unreachable("ssh: no route".into())))));
    let value = servers::status(&config, &server()).unwrap();
    set_fake_command(None);
    assert_eq!(value["stale"], true);
    assert_eq!(value["error"], "unreachable");
    assert!(value["fetched_at"].as_f64().unwrap() > 0.0);
    assert_eq!(value["models"][0]["state"], "stopped");
    // Without a cache the failure is the answer, its kind kept.
    let other = Server { name: "other".into(), ..server() };
    set_fake_command(Some(Box::new(|_: &Server, _: &[String], _| Err(Failure::TimedOut("20 s".into())))));
    assert_eq!(servers::status(&config, &other).unwrap_err().value()["error"], "timed_out");
    set_fake_command(None);
}

#[test]
fn each_agent_gets_its_wire_s_api_name_base_url_and_the_live_window() {
    let server = server();
    let ready = catalog("strata-1m", "ready");
    let claude = resolve_in(&server, &ready, "claude", "qwen3.8-flash-next", None).unwrap();
    assert_eq!(claude.api_model, "qwen3.8-flash-next-coder");
    assert_eq!(claude.base_url, "http://192.0.2.79:4000");
    assert_eq!(claude.context, Some(1_000_000));
    assert_eq!(claude.service, "qwen3.8-flash-next");
    let codex = resolve_in(&server, &ready, "codex", "qwen3.8-flash-next", None).unwrap();
    assert_eq!((codex.api_model.as_str(), codex.base_url.as_str()), ("qwen3.8-flash-next", "http://192.0.2.79:4000/v1"));
    // A session named by an API name of the other wire still gets its own.
    assert_eq!(resolve_in(&server, &ready, "claude", "qwen3.8-flash-next", None).unwrap().api_model, "qwen3.8-flash-next-coder");
    assert_eq!(resolve_in(&server, &ready, "codex", "qwen3.8-flash-next-coder", None).unwrap().api_model, "qwen3.8-flash-next");
    // After a switch to strata the next session gets that window.
    assert_eq!(resolve_in(&server, &catalog("strata", "ready"), "claude", "qwen3.8-flash-next", None).unwrap().context, Some(262_144));
    // A manual window overrides the catalog and says so.
    let mut manual = server.clone();
    manual.contexts.insert("qwen3.8-flash-next-coder".into(), 131_072);
    let resolved = resolve_in(&manual, &ready, "claude", "qwen3.8-flash-next", None).unwrap();
    assert_eq!((resolved.context, resolved.manual_context), (Some(131_072), true));
    // No wire for the agent, or no such service: an explanation, no launch.
    let error = resolve_in(&server, &ready, "claude", "runtime:comfyui-h3img", None).unwrap_err();
    assert!(error.message.contains("no anthropic API name"), "{}", error.message);
    assert!(resolve_in(&server, &ready, "claude", "lfm2.5-8b-a1b", None).unwrap_err().message.contains("not in the catalog"));
}

#[test]
fn a_claude_launch_takes_the_coder_name_and_the_catalog_window() {
    let temp = tempfile::tempdir().unwrap();
    let config = config(temp.path());
    servers::save(&config, &[server()]).unwrap();
    let document = Rc::new(RefCell::new(catalog("strata-1m", "ready")));
    let _calls = fake(Rc::clone(&document));
    let settings = session_settings(&config, "gb10", "claude", "qwen3.8-flash-next").unwrap();
    assert!(settings.env.contains(&"ANTHROPIC_MODEL=qwen3.8-flash-next-coder".to_owned()), "{:?}", settings.env);
    assert!(settings.env.contains(&"ANTHROPIC_BASE_URL=http://192.0.2.79:4000".to_owned()));
    assert!(settings.env.contains(&"CLAUDE_CODE_MAX_CONTEXT_TOKENS=1000000".to_owned()));
    assert!(settings.env.contains(&"HGS_MODEL_SERVER_SERVICE=qwen3.8-flash-next".to_owned()));
    *document.borrow_mut() = catalog("strata", "ready");
    let settings = session_settings(&config, "gb10", "claude", "qwen3.8-flash-next").unwrap();
    assert!(settings.env.contains(&"CLAUDE_CODE_MAX_CONTEXT_TOKENS=262144".to_owned()));
    // Codex on the same row: its OpenAI name and /v1.
    let settings = session_settings(&config, "gb10", "codex", "qwen3.8-flash-next").unwrap();
    assert!(settings.args.contains(&"qwen3.8-flash-next".to_owned()));
    assert!(settings.args.iter().any(|arg| arg.contains("base_url=\"http://192.0.2.79:4000/v1\"")), "{:?}", settings.args);
    assert!(settings.args.contains(&"model_context_window=262144".to_owned()));
    set_fake_command(None);
}

#[test]
fn a_variant_is_checked_before_anything_runs_and_a_running_service_asks_first() {
    let temp = tempfile::tempdir().unwrap();
    let config = config(temp.path());
    let document = Rc::new(RefCell::new(catalog("strata-1m", "stopped")));
    let calls = fake(Rc::clone(&document));
    let server = server();
    // An unknown variant never reaches the variant command.
    let refused = variant(&config, &server, "qwen3.8-flash-next", "q2", false, false, false).unwrap_err();
    assert!(refused.value()["detail"].as_str().unwrap().contains("no variant q2"));
    assert!(calls.borrow().iter().all(|argv| argv[1] == "catalog"));
    // A stopped service changes its selection at once.
    let receipt = variant(&config, &server, "qwen3.8-flash-next", "strata", false, false, false).unwrap();
    assert_eq!((receipt["status"].as_str(), receipt["restarted"].as_bool()), (Some("ran"), Some(false)));
    assert_eq!(calls.borrow().last().unwrap(), &["gb10-model", "engine", "qwen3.8-flash-next", "strata"]);
    // auto clears the manual choice.
    variant(&config, &server, "qwen3.8-flash-next", "auto", false, false, false).unwrap();
    // A running service needs --restart, then a confirmation, then restarts.
    *document.borrow_mut() = catalog("strata-1m", "ready");
    let refused = variant(&config, &server, "qwen3.8-flash-next", "strata", false, false, false).unwrap_err();
    assert!(refused.value()["detail"].as_str().unwrap().contains("--restart"));
    let before = calls.borrow().len();
    let confirm = variant(&config, &server, "qwen3.8-flash-next", "strata", true, false, false).unwrap();
    assert_eq!(confirm["status"], "confirm");
    assert!(confirm["sessions"].is_array());
    assert!(calls.borrow()[before..].iter().all(|argv| argv[1] == "catalog"));
    let receipt = variant(&config, &server, "qwen3.8-flash-next", "strata", true, true, false).unwrap();
    assert_eq!(receipt["restarted"], true);
    let tail: Vec<String> = calls.borrow().iter().rev().take(3).map(|argv| argv[1].clone()).collect();
    assert_eq!(tail, ["up", "engine", "down"]);
    // A service outside the server's control offers no variant.
    let refused = variant(&config, &server, "runtime:comfyui-h3img", "auto", false, false, false).unwrap_err();
    assert!(refused.value()["detail"].as_str().unwrap().contains("outside"));
    set_fake_command(None);
}

#[test]
fn sessions_use_a_service_by_any_api_name_or_their_account_s_gateway() {
    let temp = tempfile::tempdir().unwrap();
    let server = server();
    let ready = catalog("strata-1m", "ready");
    let home = temp.path().join("gb10-claude");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("settings.json"), json!({"env": {"ANTHROPIC_BASE_URL": "http://192.0.2.79:4000/", "ANTHROPIC_MODEL": "qwen3.8-flash-next-coder"}}).to_string()).unwrap();
    let records = vec![
        ("claude/a/coder".to_owned(), json!({"model_server": {"server": "gb10", "model": "qwen3.8-flash-next-coder"}})),
        ("codex/a/openai".to_owned(), json!({"model_server": {"server": "gb10", "model": "qwen3.8-flash-next"}})),
        ("claude/a/account".to_owned(), json!({"agent_home": home.display().to_string()})),
        ("claude/a/cloud".to_owned(), json!({})),
        ("codex/a/other".to_owned(), json!({"model_server": {"server": "lab", "model": "small"}})),
    ];
    let sessions = sessions_using(&server, &ready, records.into_iter());
    let names: Vec<(&str, &str, &str)> = sessions
        .iter()
        .map(|row| (row["name"].as_str().unwrap(), row["service"].as_str().unwrap(), row["via"].as_str().unwrap()))
        .collect();
    assert_eq!(names, [
        ("claude/a/coder", "qwen3.8-flash-next", "server"),
        ("codex/a/openai", "qwen3.8-flash-next", "server"),
        ("claude/a/account", "qwen3.8-flash-next", "account"),
    ]);
}

#[test]
fn the_catalog_key_stays_private_and_the_row_is_validated() {
    let mut row = server();
    row.api = Some(CatalogApi { base_url: "http://192.0.2.79:4010/".into(), key: "secret-token".into() });
    let row = checked(row).unwrap();
    assert_eq!(row.api.as_ref().unwrap().base_url, "http://192.0.2.79:4010");
    let shown = public(&row).to_string();
    assert!(!shown.contains("secret-token"), "{shown}");
    assert!(shown.contains("\"api\":{\"base_url\":\"http://192.0.2.79:4010\",\"key\":true}"), "{shown}");
    // The variant command names its variant; an unknown protocol is refused;
    // catalog fields need the protocol.
    let mut bad = server();
    bad.variant = vec!["gb10-model".into(), "engine".into(), "{model}".into()];
    assert!(checked(bad).unwrap_err().message.contains("{variant}"));
    let mut bad = server();
    bad.protocol = "model-catalog/9".into();
    assert!(checked(bad).is_err());
    let mut bad = server();
    bad.protocol.clear();
    assert!(checked(bad).unwrap_err().message.contains("need protocol"));
    // A plain row keeps working as before.
    let plain = checked(Server { name: "lab".into(), base_url: "http://127.0.0.1:8080".into(), kind: "openai".into(), ..Default::default() }).unwrap();
    assert!(!is_catalog(&plain));
}
