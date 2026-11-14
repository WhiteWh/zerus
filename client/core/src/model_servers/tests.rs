use super::*;
use serde_json::json;

fn list() -> Value {
    json!({"servers": [
        {"name": "lab", "base_url": "http://lab.example:8000/v1/", "kind": "openai", "ssh": "gpu", "key": true,
         "command": {"status": true, "up": true, "down": true, "logs": true}, "up_minutes": 20,
         "argv": {"status": ["serve", "status"], "up": ["serve", "up", "{model}"], "down": ["serve", "down", "{model}"], "logs": ["journalctl", "-u", "my unit"]},
         "contexts": {"small": 32768}},
        {"name": "cloud", "base_url": "https://api.example", "kind": "anthropic", "ssh": "", "key": false,
         "argv": {"status": [], "up": [], "down": [], "logs": []}, "contexts": {}},
        {"name": "", "base_url": "x"}]})
}
fn lab() -> Server {
    parse_list(&list()).remove(0)
}

#[test]
fn every_call_is_this_computers_model_server_with_one_argument_per_word() {
    let args = |op: Op| op.invocation().unwrap().args;
    assert_eq!(args(Op::List), ["model-server", "list", "--json"]);
    assert_eq!(
        args(Op::Status {
            server: "lab".into()
        }),
        ["model-server", "status", "lab", "--json"]
    );
    assert_eq!(
        args(Op::Up {
            server: "lab".into(),
            model: "small".into()
        }),
        ["model-server", "up", "lab", "small", "--detach"]
    );
    assert_eq!(
        args(Op::Down {
            server: "lab".into(),
            model: "small".into()
        }),
        ["model-server", "down", "lab", "small", "--yes"]
    );
    assert_eq!(
        args(Op::Logs {
            server: "lab".into(),
            model: String::new()
        }),
        ["model-server", "logs", "lab", "--lines", "200"]
    );
    assert_eq!(
        args(Op::Sessions {
            server: "lab".into()
        }),
        ["model-server", "sessions", "lab", "--json"]
    );
    let id = "11111111-1111-4111-8111-111111111111";
    assert_eq!(
        args(Op::UpResult { id: id.into() }),
        ["model-server", "up-result", id, "--json"]
    );
    let save = Op::Save {
        servers: vec![json!({"name": "lab"})],
    }
    .invocation()
    .unwrap();
    let input: Value = serde_json::from_slice(save.stdin.as_ref().unwrap()).unwrap();
    assert_eq!(input, json!({"by": "human", "servers": [{"name": "lab"}]}));
    assert!(Op::Up {
        server: "-x".into(),
        model: "m".into()
    }
    .invocation()
    .is_err());
    assert!(Op::Down {
        server: "lab".into(),
        model: String::new()
    }
    .invocation()
    .is_err());
    assert!(Op::UpResult { id: "nope".into() }.invocation().is_err());
    assert!(Op::Save {
        servers: vec![json!({}); 65]
    }
    .invocation()
    .is_err());
    assert!(
        !args(Op::List).iter().any(|a| a.starts_with('@')),
        "never another computer"
    );
}

#[test]
fn a_json_reply_is_the_answer_whatever_the_exit() {
    let exit = |code, out: &str| Exit::Code {
        code,
        stdout: out.as_bytes().to_vec(),
        stderr: "boom".into(),
    };
    assert_eq!(
        reply(exit(1, r#"{"status":"error","error":"unreachable"}"#)).unwrap()["error"],
        "unreachable"
    );
    assert_eq!(reply(exit(1, "not json")).unwrap_err(), "boom");
    assert!(reply(exit(0, "")).is_err());
    assert_eq!(reply(Exit::TimedOut).unwrap_err(), "timed out");
}

#[test]
fn the_list_keeps_argv_contexts_and_derives_the_urls() {
    let servers = parse_list(&list());
    assert_eq!(servers.len(), 2, "a row without a name is dropped");
    let lab = &servers[0];
    assert!(lab.has_key && !lab.read_only);
    assert_eq!(lab.agent_url, "http://lab.example:8000/v1");
    assert_eq!(lab.models_url, "http://lab.example:8000/v1/models");
    assert_eq!(lab.logs_text, "journalctl -u \"my unit\"");
    assert!(servers[1].read_only);
    assert_eq!(servers[1].agent_url, "https://api.example");
    let json = lab.json();
    assert_eq!(json["up_minutes"], 20);
    assert_eq!(json["contexts"], json!({"small": 32768}));
    assert_eq!(json["up"], json!(["serve", "up", "{model}"]));
    assert!(json.get("key").is_none(), "never a key");
    assert!(servers[1].json().get("status").is_none());
}

#[test]
fn argv_text_round_trips_like_qprocess() {
    assert_eq!(
        argv_from_text(r#"  serve up "{model}" --name "my model"  "#),
        ["serve", "up", "{model}", "--name", "my model"]
    );
    assert_eq!(
        argv_from_text(r#"echo """quoted""""#),
        ["echo", "\"quoted\""]
    );
    assert!(argv_from_text("   ").is_empty());
    let argv: Vec<String> = vec!["a".into(), "b c".into(), String::new()];
    assert_eq!(argv_from_text(&argv_text(&argv))[..2], argv[..2]);
}

#[test]
fn a_status_read_names_its_models_memory_and_actions() {
    let status = parse_status(
        &lab(),
        Ok(&json!({"source": "command", "models": [
        {"name": "small", "state": "RUNNING", "since": 1793900000.0},
        {"name": "large", "state": "stopped"},
        {"name": "mid", "state": "loading"},
        {"name": "", "state": "running"}],
        "memory": {"free": 6.0, "total": 48.0}, "raw": ["something else"]})),
    );
    assert_eq!(status.models.len(), 3);
    assert_eq!(status.models[0].state, "running");
    assert!(status.models[0].usable && status.models[0].can_down && !status.models[0].can_up);
    assert!(status.models[1].can_up && !status.models[1].usable);
    assert_eq!(status.models[2].label.english(), "Loading");
    assert_eq!(status.memory.english(), "6.0 of 48.0 GB free");
    assert!(status.low);
    assert_eq!(status.running, 1);
    assert_eq!(status.active, ["small", "mid"]);
    assert_eq!(status.raw, ["something else"]);
    // The API's list: listed, usable, no Up or Down.
    let api = parse_status(
        &parse_list(&list())[1],
        Ok(&json!({"source": "api", "models": [{"name": "m"}]})),
    );
    assert_eq!(api.models[0].state, "listed");
    assert!(api.models[0].usable && !api.models[0].can_down);
    assert!(api.memory.is_empty());
    assert_eq!(state_label("needs_auth", "command").english(), "Needs auth");
}

#[test]
fn a_failed_read_says_why_and_that_nothing_retries() {
    let refused = parse_status(
        &lab(),
        Ok(&json!({"status": "error", "error": "unauthorized", "detail": "401"})),
    );
    assert_eq!(refused.error_label.english(), "Key refused");
    assert_eq!(
        refused.problem.english(),
        "The server refused the API key: 401. Zerus tries again only when you ask."
    );
    let read_only = parse_list(&list()).remove(1);
    let unreachable = parse_status(
        &read_only,
        Ok(&json!({"status": "error", "error": "unreachable", "detail": "connection refused."})),
    );
    assert_eq!(unreachable.problem.english(), "GET https://api.example/v1/models: connection refused. Zerus tries again only when you ask.");
    let failed = parse_status(&read_only, Err("hgs: boom"));
    assert_eq!(failed.error, "failed");
    assert_eq!(
        failed.problem.english(),
        "hgs: boom. Zerus tries again only when you ask."
    );
}

#[test]
fn up_reports_its_start_then_its_own_outcome() {
    let started = up_started(
        "lab",
        "small",
        Ok(&json!({"status": "started", "request_id": "r1", "timeout_minutes": 10})),
    );
    assert!(started.started);
    assert_eq!(started.limit_ms, 10 * 60_000 + 120_000);
    let refused = up_started(
        "lab",
        "small",
        Ok(&json!({"status": "refused", "error": "already running"})),
    );
    assert_eq!(
        refused.outcome.unwrap().text.english(),
        "Up small on lab failed: already running"
    );
    assert!(
        up_poll(
            "lab",
            "small",
            "r1",
            Ok(&json!({"status": "running"})),
            false
        )
        .running
    );
    let expired = up_poll(
        "lab",
        "small",
        "r1",
        Ok(&json!({"status": "running"})),
        true,
    );
    assert_eq!(expired.outcome.unwrap().text.english(), "Up small on lab failed: No result within its time limit; it may still be running (hgs run r1)");
    let ok = up_poll(
        "lab",
        "small",
        "r1",
        Ok(&json!({"status": "ran", "exit": 0})),
        false,
    )
    .outcome
    .unwrap();
    assert!(!ok.error);
    assert_eq!(
        ok.text.english(),
        "Up small on lab finished: exit 0. Nothing starts it again on its own."
    );
    let exit = up_outcome(
        "lab",
        "small",
        Ok(&json!({"status": "ran", "exit": 2, "tail": "loading\nout of memory\n\n"})),
    );
    assert_eq!(
        exit.text.english(),
        "Up small on lab failed: exit 2: out of memory"
    );
    let timed = up_outcome(
        "lab",
        "small",
        Ok(&json!({"status": "ran", "exit": -1, "timed_out": true, "timeout_secs": 1800})),
    );
    assert_eq!(
        timed.text.english(),
        "Up small on lab failed: stopped at its 30 min limit"
    );
}

#[test]
fn down_sessions_and_logs_replies() {
    assert!(down_failure(Ok(&json!({"status": "ran", "exit": 0}))).is_none());
    assert_eq!(
        down_failure(Ok(&json!({"status": "ran", "exit": 3})))
            .unwrap()
            .english(),
        "exit 3"
    );
    assert_eq!(
        down_failure(Ok(&json!({"status": "error", "detail": "no ssh"})))
            .unwrap()
            .english(),
        "no ssh"
    );
    let by = sessions_by_model(Ok(&json!({"sessions": [{"name": "codex/a/x", "model": "small"}, {"name": "claude/b/y", "model": "small"}]}))).unwrap();
    assert_eq!(by["small"], ["codex/a/x", "claude/b/y"]);
    assert!(sessions_by_model(Err("timed out")).is_err());
    let read = logs(Ok(
        &json!({"status": "ran", "lines": ["a", "b"], "truncated": true}),
    ))
    .unwrap();
    assert_eq!(
        read,
        Logs {
            lines: vec!["a".into(), "b".into()],
            truncated: true
        }
    );
}

#[test]
fn settings_saves_the_whole_list_and_keeps_keys_unless_changed() {
    let stored = parse_list(&list());
    let form = Form {
        was: "lab".into(),
        name: "lab2".into(),
        base_url: " http://lab.example:9000 ".into(),
        kind: "openai".into(),
        ssh: "gpu".into(),
        status: "serve status".into(),
        up: "serve up {model}".into(),
        ..Default::default()
    };
    let list = saved_list(&stored, &form);
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["name"], "lab2");
    assert_eq!(list[0]["was"], "lab");
    assert_eq!(list[0]["base_url"], "http://lab.example:9000");
    assert!(list[0].get("key").is_none(), "the stored key stays");
    assert_eq!(list[0]["contexts"], json!({"small": 32768}));
    assert_eq!(list[0]["up_minutes"], 20);
    assert!(list[0].get("down").is_none());
    assert_eq!(list[1]["was"], "cloud");
    let removed = Form {
        key_change: "remove".into(),
        ..form.clone()
    };
    assert_eq!(removed.server(&stored, true)["key"], Value::Null);
    let replaced = Form {
        key_change: "replace".into(),
        key: "sk-new".into(),
        ..form.clone()
    };
    assert_eq!(replaced.server(&stored, true)["key"], "sk-new");
    let new = Form {
        name: "new".into(),
        base_url: "http://x".into(),
        ..Default::default()
    };
    assert_eq!(saved_list(&stored, &new).len(), 3);
    assert_eq!(new.server(&stored, false)["kind"], "openai");
    assert_eq!(without(&stored, "lab").len(), 1);
    assert_eq!(save_refusal(Ok(&json!({"status": "saved"}))), None);
    assert_eq!(
        save_refusal(Err("Server lab: name is already used"))
            .unwrap()
            .english(),
        "Server lab: name is already used"
    );
}

#[test]
fn test_results_read_like_qt() {
    let api = test_result(
        "api",
        "http://lab.example:8000/v1",
        Ok(&json!({"source": "api", "models": [{"name": "a"}, {"name": "b"}]})),
    );
    assert_eq!(
        api.text.english(),
        "GET http://lab.example:8000/v1/models answered: 2 models"
    );
    let failed = test_result(
        "api",
        "http://x",
        Ok(&json!({"status": "error", "detail": "connection refused"})),
    );
    assert_eq!(
        (failed.tone.as_str(), failed.text.english()),
        ("error", "GET http://x/v1/models: connection refused".into())
    );
    let raw = test_result(
        "status",
        "",
        Ok(&json!({"source": "command", "raw": ["?"]})),
    );
    assert_eq!(raw.tone, "warn");
    assert_eq!(raw.raw, ["?"]);
    let ok = test_result(
        "status",
        "",
        Ok(
            &json!({"source": "command", "models": [{"name": "a", "state": "running"}], "memory": {"free": 1, "total": 2}}),
        ),
    );
    assert_eq!(ok.text.english(), "1 model, 1.0 of 2.0 GB free");
    let stored = lab();
    assert_eq!(
        logs_test(Some(&stored), "other", "small", None)
            .text
            .english(),
        "Save the server first: Logs reads the saved command"
    );
    assert_eq!(
        logs_test(Some(&stored), &stored.logs_text, "", None)
            .text
            .english(),
        "Read the status first: Logs needs one of the server's models"
    );
    let read = logs_test(
        Some(&stored),
        &stored.logs_text,
        "small",
        Some(Ok(&json!({"lines": ["x"]}))),
    );
    assert_eq!(read.text.english(), "small: 1 lines read");
}

#[test]
fn a_session_starts_through_hgs_with_the_server_and_model() {
    let id = "11111111-1111-4111-8111-111111111111";
    let call = session_launch("codex", r"C:\work\app", "small", "lab", "small", id).unwrap();
    assert_eq!(
        call.args,
        [
            "codex",
            r"C:\work\app",
            "--new",
            "-n",
            "small",
            "--launch-id",
            id,
            "-d",
            "--model-server",
            "lab",
            "--model",
            "small"
        ]
    );
    assert!(session_launch("opencode", "/w", "t", "lab", "m", id).is_err());
    assert!(session_launch("codex", "", "t", "lab", "m", id).is_err());
    assert!(session_launch("codex", "/w", "-t", "lab", "m", id).is_err());
    assert!(session_launch("codex", "/w", "t", "lab", "m", "x").is_err());
}

/// Owner spec 2026-11-14: a model-catalog/2 status carries each service's
/// variants, window, API names and actions; ready reads as running; a service
/// outside the server's control offers nothing; a stale read keeps its rows.
#[test]
fn a_catalog_status_reads_services_with_their_variants_and_actions() {
    let row = json!({"name": "gb10", "base_url": "http://gb10.example:4000", "kind": "anthropic", "ssh": "gb10",
        "protocol": "model-catalog/2", "api": {"base_url": "http://gb10.example:4010", "key": true},
        "argv": {"status": [], "up": ["gb10-model", "up", "{model}"], "down": ["gb10-model", "down", "{model}"], "logs": [],
                 "catalog": ["gb10-model", "catalog"], "variant": ["gb10-model", "engine", "{model}", "{variant}"]}});
    let server = parse_list(&json!({"servers": [row]})).remove(0);
    assert!(server.catalog_server() && server.has_api_key && !server.read_only);
    assert_eq!(server.json()["api"], json!({"base_url": "http://gb10.example:4010"}));
    assert_eq!(server.json()["variant"], json!(["gb10-model", "engine", "{model}", "{variant}"]));
    let status = json!({"source": "catalog", "stale": true, "error": "unreachable", "detail": "no route", "fetched_at": 1794621120.0,
        "memory": {"free": 14.2, "total": 121.6, "unit": "GiB"},
        "models": [
            {"name": "qwen", "state": "ready", "managed": true, "variant": "strata-1m", "context": 1000000, "context_max": 1000000,
             "memory_gib": 58.6, "needs_gib": 100.0, "speed": {"tok_s": 42.2, "code_tok_s": 64.0},
             "variants": [{"id": "strata", "context": 262144, "needs_gib": 100, "tok_s": 46.1, "code_tok_s": 72.2, "selected": false, "running": false},
                          {"id": "strata-1m", "context": 1000000, "needs_gib": 100, "tok_s": 42.2, "code_tok_s": 64.0, "selected": true, "running": true}],
             "api_models": [{"id": "qwen", "wire": "openai"}, {"id": "qwen-coder", "wire": "anthropic"}],
             "actions": {"up": false, "down": true, "variant": true, "logs": true}},
            {"name": "glm", "state": "stopped", "managed": true, "fit": "fits", "actions": {"up": true, "down": false, "variant": true}},
            {"name": "runtime:comfy", "state": "external", "managed": false, "actions": {"up": false, "down": false, "variant": false}}]});
    let read = parse_status(&server, Ok(&status));
    assert!(read.stale);
    assert_eq!((read.error.as_str(), read.fetched_at), ("unreachable", 1794621120));
    let qwen = &read.models[0];
    assert_eq!(qwen.state, "running");
    assert!(qwen.usable && qwen.can_down && !qwen.can_up);
    let facts = qwen.catalog.as_ref().unwrap();
    assert_eq!((facts.variant.as_str(), facts.context, facts.variants.len()), ("strata-1m", 1_000_000, 2));
    assert_eq!(facts.api_models[1].wire, "anthropic");
    assert!(facts.can_variant);
    assert!(read.models[1].can_up && !read.models[1].can_down);
    let comfy = &read.models[2];
    assert!(!comfy.usable && !comfy.can_up && !comfy.can_down && !comfy.catalog.as_ref().unwrap().can_variant);
    // A plain status has no catalog facts.
    assert!(parse_status(&server, Ok(&json!({"source": "command", "models": [{"name": "x", "state": "running"}]}))).models[0].catalog.is_none());
}

#[test]
fn a_variant_change_is_one_confirmed_call() {
    let op = Op::Variant { server: "gb10".into(), model: "qwen".into(), variant: "strata".into(), restart: true };
    assert_eq!(op.invocation().unwrap().args, ["model-server", "variant", "gb10", "qwen", "strata", "--yes", "--detach", "--restart"]);
    let stopped = Op::Variant { server: "gb10".into(), model: "glm".into(), variant: "auto".into(), restart: false };
    assert!(!stopped.invocation().unwrap().args.contains(&"--restart".to_owned()));
    assert!(Op::Variant { server: "gb10".into(), model: "qwen".into(), variant: "-x".into(), restart: false }.invocation().is_err());
    // Sessions group by their service, whichever API name they use.
    let by = sessions_by_model(Ok(&json!({"sessions": [
        {"name": "claude/a/x", "model": "qwen-coder", "service": "qwen"},
        {"name": "codex/a/y", "model": "qwen", "service": "qwen"},
        {"name": "codex/a/z", "model": "small"}]}))).unwrap();
    assert_eq!(by["qwen"], ["claude/a/x", "codex/a/y"]);
    assert_eq!(by["small"], ["codex/a/z"]);
}
