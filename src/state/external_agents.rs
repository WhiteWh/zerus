//! Nested agent CLIs and native Codex children inherit the HGS run token. Their
//! SessionStart must never rebind the parent conversation.
use super::*;
use std::io::{BufRead, BufReader};

#[derive(Clone, Copy)]
struct Caller {
    provider: &'static str,
    headless: bool,
}

fn native_caller(argv: &[String]) -> Option<Caller> {
    let executable = crate::platform::program_name(Path::new(argv.first()?))?;
    // macOS framework builds run python3 scripts as .../Python.app/Contents/MacOS/Python.
    let offset = if executable == "node" || executable.to_ascii_lowercase().starts_with("python") { 1 } else { 0 };
    let name = crate::platform::program_name(Path::new(argv.get(offset)?))?;
    let provider = match name.as_str() {
        "codex" | "codex.exe" | "codex.js" => "codex",
        "claude" | "claude.exe" => "claude",
        "kimi" => "kimi",
        _ => return None,
    };
    let mut args = argv.iter().skip(offset + 1);
    let headless = provider == "codex" && loop {
        let Some(arg) = args.next() else { break false; };
        if arg == "--" { break false; }
        if ["-c", "--config", "-C", "--cd", "-m", "--model", "-p", "--profile",
            "-s", "--sandbox", "-a", "--ask-for-approval", "--enable", "--disable",
            "--add-dir", "--local-provider"].contains(&arg.as_str()) {
            args.next();
        } else if arg.starts_with('-') {
            // Known switches and --option=value cannot consume the next word.
            if !arg.contains('=') && !["--no-daemon", "--no-alt-screen", "--oss", "--search",
                "--dangerously-bypass-hook-trust", "--dangerously-bypass-approvals-and-sandbox",
                "--full-auto"].contains(&arg.as_str()) { break false; }
        } else { break ["exec", "e", "review", "app-server"].contains(&arg.as_str()); }
    };
    Some(Caller { provider, headless })
}

fn ancestry(root: u32, mut pid: u32, mut read: impl FnMut(u32) -> Option<(u32, Vec<String>)>) -> Option<Caller> {
    let mut caller = None;
    for _ in 0..64 {
        let (parent, argv) = read(pid)?;
        if caller.is_none() { caller = native_caller(&argv); }
        if pid == root { return caller; }
        if parent == pid || parent <= 1 { return None; }
        pid = parent;
    }
    None
}

fn caller(record: &Value) -> Option<Caller> {
    if !process_alive(record) { return None; }
    ancestry(record["pid"].as_u64()? as u32, crate::platform::parent_process_id(), crate::platform::parent_and_argv)
}

pub(super) fn main_codex_hook(record: &Value) -> bool {
    caller(record).is_some_and(|source| source.provider == "codex" && !source.headless)
}

// An inherited HGS token is not proof that a new same-provider conversation
// belongs to the interactive parent. Codex records its headless entry point in
// the rollout header; /new and /clear still use the interactive source.
fn codex_metadata(event: &Value) -> Option<Value> {
    let path = Path::new(string(event, "transcript_path"));
    if !path.is_absolute() { return None; }
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file.take(256 * 1024)).read_line(&mut line).ok()?;
    if !line.ends_with('\n') { return None; }
    let meta: Value = serde_json::from_str(&line).ok()?;
    (meta["type"] == "session_meta")
        .then(|| meta["payload"].clone())
}

fn codex_subagent(meta: &Value) -> bool {
    meta["source"].get("subagent").is_some()
}

fn codex_parent(meta: &Value, parent: &str) -> bool {
    let parents: Vec<_> = [meta.get("parent_thread_id"),
        meta["source"]["subagent"]["thread_spawn"].get("parent_thread_id")]
        .into_iter().flatten().collect();
    !parent.is_empty() && !parents.is_empty() && parents.iter().all(|id| id.as_str() == Some(parent))
}

fn provider(event: &Value) -> Option<&'static str> {
    let id = string(event, "session_id");
    if id
        .strip_prefix("session_")
        .is_some_and(|v| uuid::Uuid::parse_str(v).is_ok())
    {
        return Some("kimi");
    }
    let path = Path::new(string(event, "transcript_path"));
    if path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("rollout-"))
    {
        return Some("codex");
    }
    let claude = nonempty_env("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".claude"));
    if path.is_absolute() && path.starts_with(claude) && uuid::Uuid::parse_str(id).is_ok() {
        return Some("claude");
    }
    None
}

/// An unidentified inherited SessionStart cannot replace an established parent.
/// Keep legacy startup hooks and verified interactive /new and /clear events.
pub(super) fn main_start(record: &Value, event: &Value) -> bool {
    if codex_metadata(event).as_ref().is_some_and(|meta| meta["id"] != event["session_id"] || codex_subagent(meta))
        || (record["agent"] == "codex" && record["subagents"].get(string(event, "session_id"))
            .is_some_and(|child| child.is_object() && child["external"] != true)) {
        return false;
    }
    if event["session_id"] == record["conversation_id"] { return true; }
    match provider(event).or_else(|| caller(record).map(|source| source.provider)) {
        Some(provider) => provider == string(record, "agent"),
        None => string(record, "conversation_id").is_empty(),
    }
}

/// Return a child-scoped event, without touching parent identity, model, or
/// native transcript. Unknown foreign conversations cannot claim a child.
pub(super) fn route(record: &mut Value, event: &Value) -> Option<Value> {
    let sid = string(event, "session_id");
    if sid.is_empty()
        || sid.len() > 100
        || sid.contains(['/', '\\'])
        || sid.chars().any(char::is_control)
        || string(record, "conversation_id").is_empty()
        || sid == string(record, "conversation_id")
    {
        return None;
    }
    let metadata = codex_metadata(event);
    if metadata.as_ref().is_some_and(|meta| meta["id"] != event["session_id"]) { return None; }
    if metadata.as_ref().is_some_and(|meta| codex_subagent(meta)
        && !codex_parent(meta, string(record, "conversation_id"))) { return None; }
    let native = record["agent"] == "codex" && (
        metadata.as_ref().is_some_and(|meta| codex_subagent(meta)
            && codex_parent(meta, string(record, "conversation_id")))
        || record["subagents"].get(sid).is_some_and(|child| child.is_object() && child["external"] != true));
    // A native child's mismatched or missing parent is never a new root or an
    // external review. Its native ID also keeps SubagentStart/Stop in one row.
    if metadata.as_ref().is_some_and(codex_subagent) && !native { return None; }
    let known = record["subagents"].as_object().and_then(|children| {
        children
            .iter()
            .find(|(_, child)| child["external"] == true && child["conversation_id"] == sid)
            .map(|(id, _)| id.clone())
    });
    let id = if native {
        if !record["subagents"].is_object() { record["subagents"] = json!({}); }
        if !record["subagents"][sid].is_object() {
            record["subagents"][sid] = json!({"name":format!("codex {}", sid.chars().take(8).collect::<String>()), "started":now()});
        }
        sid.to_owned()
    } else if let Some(id) = known {
        id
    } else {
        if string(event, "hook_event_name") != "SessionStart" {
            return None;
        }
        let source = caller(record);
        let agent = provider(event).or_else(|| source.map(|source| source.provider))?;
        let headless = agent == "codex" && (metadata.as_ref().is_some_and(|meta| meta["source"] == "exec")
            || source.is_some_and(|source| source.provider == "codex" && source.headless));
        if agent == string(record, "agent") && !headless {
            return None;
        }
        let id = format!("external:{agent}:{sid}");
        let agent_home = nonempty_env(home_var(agent).ok()?)
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                home().join(if agent == "kimi" {
                    ".kimi-code"
                } else if agent == "codex" {
                    ".codex"
                } else {
                    ".claude"
                })
            });
        if !record["subagents"].is_object() {
            record["subagents"] = json!({});
        }
        record["subagents"][&id] = json!({"external":true,"provider":agent,"conversation_id":sid,
            "name":format!("{} {}",agent,sid.trim_start_matches("session_").chars().take(8).collect::<String>()),
            "started":now(),"transcript":event["transcript_path"],"cwd":event["cwd"],"agent_home":agent_home});
        id
    };
    let child = &mut record["subagents"][&id];
    for (from, to) in [
        ("cwd", "cwd"),
        ("transcript_path", "transcript"),
        ("model", "model"),
        ("reasoning_effort", "effort"),
        ("effort", "effort"),
    ] {
        if !string(event, from).is_empty() {
            child[to] = event[from].clone();
        }
    }
    let mut scoped = event.clone();
    scoped["agent_id"] = json!(id);
    // Keep the registered label: a native agent_type such as "default" belongs
    // to the foreign harness, not to the parent provider's profile registry.
    scoped["agent_type"] = child["name"].clone();
    Some(scoped)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn caller_requires_ancestry_to_the_owned_parent() {
        let rows = [
            (50, (40, vec!["sh", "-c", "hgs __state hook"])),
            (40, (30, vec!["/opt/codex", "exec", "--ephemeral"])),
            (30, (20, vec!["bash", "-c", "review"])),
            (20, (1, vec!["/opt/claude"])),
        ];
        let read = |pid| rows.iter().find(|(id, _)| *id == pid).map(|(_, (parent, argv))|
            (*parent, argv.iter().map(|arg| arg.to_string()).collect()));
        let child = ancestry(20, 50, read).unwrap();
        assert_eq!(child.provider, "codex");
        assert!(child.headless);
        assert!(ancestry(21, 50, read).is_none());
        assert!(ancestry(20, 51, read).is_none());
        let parent = ancestry(20, 20, read).unwrap();
        assert_eq!(parent.provider, "claude");
        assert!(!parent.headless);
        let wrapper = |pid| Some((if pid == 20 { 1 } else { 20 },
            vec!["node".into(), "/opt/bin/codex.js".into(), "--".into(), "exec".into()]));
        assert!(!ancestry(20, 30, wrapper).unwrap().headless);
        assert!(ancestry(20, 30, |pid| Some((pid, vec![]))).is_none());
        for (argv, headless) in [
            (vec!["codex", "-m", "exec"], false),
            (vec!["codex", "--no-daemon", "-c", "model=fixture", "exec", "--ephemeral"], true),
            (vec!["node", "/opt/codex.js", "resume", "exec"], false),
        ] {
            assert_eq!(native_caller(&argv.into_iter().map(str::to_owned).collect::<Vec<_>>()).unwrap().headless, headless);
        }
    }

    #[test]
    fn macos_framework_python_hosts_agents() {
        // macOS ps reports a python3 shebang script under the framework executable.
        let argv = ["/Library/Frameworks/Python.framework/Versions/3.13/Resources/Python.app/Contents/MacOS/Python",
            "/opt/bin/claude"].map(str::to_owned);
        assert_eq!(native_caller(&argv).unwrap().provider, "claude");
    }

    #[test]
    fn unidentified_start_preserves_established_parent_but_allows_legacy_bootstrap() {
        let parent = json!({"agent":"claude","conversation_id":"parent"});
        let unknown = json!({"session_id":"foreign"});
        assert!(!main_start(&parent, &unknown));
        assert!(main_start(&json!({"agent":"claude"}), &unknown));
        assert!(main_start(&parent, &json!({"session_id":"parent"})));
        assert!(!main_start(&parent, &json!({"session_id":"foreign", "transcript_path":"/tmp/rollout-foreign.jsonl"})));
    }
    #[test]
    fn nested_codex_exec_cannot_replace_the_interactive_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let sid = uuid::Uuid::new_v4().to_string();
        let path = dir.path().join(format!("rollout-{sid}.jsonl"));
        let mut meta = json!({"type":"session_meta","payload":{"id":sid,"source":"exec"}});
        std::fs::write(&path,meta.to_string()+"\n").unwrap();
        let parent = json!({"agent":"codex","conversation_id":"parent","transcript":"parent-rollout","model":"parent-model","subagents":{}});
        let event = json!({"hook_event_name":"SessionStart","session_id":sid,"transcript_path":path,"cwd":"/child","model":"child-model"});
        let mut record = parent.clone();
        let scoped = route(&mut record,&event).unwrap();
        let child = string(&scoped,"agent_id");
        assert_eq!(record["conversation_id"],"parent");
        assert_eq!(record["transcript"],"parent-rollout");
        assert_eq!(record["model"],"parent-model");
        assert_eq!(record["subagents"][child]["provider"],"codex");
        let stop = json!({"hook_event_name":"Stop","session_id":sid,"last_assistant_message":"child finished"});
        let scoped = route(&mut record,&stop).unwrap();
        journal::update_activity(&mut record,&scoped);
        assert_eq!(record["subagents"][child]["state"],"finished");
        assert_eq!(record["conversation_id"],"parent");
        // A real /new still replaces the parent's conversation. Neither an
        // unrelated header nor a first headless launch can claim a child.
        for source in ["cli","vscode"] {
            meta["payload"]["source"]=json!(source);
            std::fs::write(&path,meta.to_string()+"\n").unwrap();
            assert!(route(&mut parent.clone(),&event).is_none());
        }
        meta["payload"]["source"]=json!("exec");
        meta["payload"]["id"]=json!("wrong");
        std::fs::write(&path,meta.to_string()+"\n").unwrap();
        assert!(route(&mut parent.clone(),&event).is_none());
        meta["payload"]["id"]=json!(sid);
        std::fs::write(&path,meta.to_string()+"\n").unwrap();
        let mut fresh=parent.clone();fresh["conversation_id"]=Value::Null;
        assert!(route(&mut fresh,&event).is_none());
    }

    #[test]
    fn native_codex_children_keep_the_parent_and_their_native_identity() {
        let dir = tempfile::tempdir().unwrap();
        let sid = uuid::Uuid::new_v4().to_string();
        let parent_id = uuid::Uuid::new_v4().to_string();
        let path = dir.path().join(format!("rollout-{sid}.jsonl"));
        for (source, announced) in [
            (json!({"subagent":{"thread_spawn":{"parent_thread_id":parent_id,"depth":1}}}), true),
            (json!({"subagent":{"other":"guardian"}}), false),
            (json!({"subagent":"review"}), false),
        ] {
            let meta = json!({"type":"session_meta","payload":{"id":sid,"parent_thread_id":parent_id,"source":source}});
            std::fs::write(&path, meta.to_string()+"\n").unwrap();
            let mut parent = json!({"agent":"codex","conversation_id":parent_id,"transcript":"parent-rollout",
                "cwd":"parent-folder","model":"parent-model","effort":"high","prompt":"Parent task",
                "last_message":"Parent reply","main_done":false,"active_tools":{"parent-tool":{}},
                "subagents":{}});
            if announced { parent["subagents"][&sid] = json!({"name":"Reviewer","started":1.0}); }
            let event = json!({"hook_event_name":"SessionStart","session_id":sid,"transcript_path":path,
                "cwd":"child-folder","model":"child-model","reasoning_effort":"low"});
            assert!(!main_start(&parent, &event));
            let mut record = parent.clone();
            let scoped = route(&mut record, &event).unwrap();
            assert_eq!(scoped["agent_id"], sid);
            if announced { assert_eq!(record["subagents"][&sid]["name"], "Reviewer"); }
            assert_eq!(record["subagents"][&sid]["model"], "child-model");
            assert_ne!(record["subagents"][&sid]["external"], true);
            journal::update_activity(&mut record, &scoped);
            // Later hooks can omit both agent_id and transcript_path. Keep the
            // same roster entry that the parent's SubagentStart/Stop uses.
            for event in [
                json!({"hook_event_name":"PreToolUse","session_id":sid,"tool_name":"shell","tool_use_id":"child-tool"}),
                json!({"hook_event_name":"Stop","session_id":sid,"last_assistant_message":"Child result"}),
            ] {
                let scoped = route(&mut record, &event).unwrap();
                assert_eq!(scoped["agent_id"], sid);
                journal::update_activity(&mut record, &scoped);
            }
            assert_eq!(record["subagents"].as_object().unwrap().len(), 1);
            assert_eq!(record["subagents"][&sid]["state"], "finished");
            assert_eq!(record["subagents"][&sid]["detail"], "Child result");
            for key in ["conversation_id","transcript","cwd","model","effort","prompt","last_message","main_done","active_tools"] {
                assert_eq!(record[key], parent[key], "{key}");
            }
            // The parent's next event is still its own work.
            let event = json!({"hook_event_name":"Stop","session_id":parent_id,"last_assistant_message":"Parent result"});
            assert!(route(&mut record, &event).is_none());
            journal::update_activity(&mut record, &event);
            assert_eq!(record["last_message"], "Parent result");
        }
    }

    #[test]
    fn native_codex_starts_require_parent_proof_and_never_bootstrap_a_root() {
        let dir = tempfile::tempdir().unwrap();
        let sid = uuid::Uuid::new_v4().to_string();
        let path = dir.path().join(format!("rollout-{sid}.jsonl"));
        let parent = json!({"agent":"codex","conversation_id":"parent","subagents":{}});
        for payload in [
            json!({"id":sid,"source":{"subagent":{"other":"guardian"}}}),
            json!({"id":sid,"parent_thread_id":"unrelated","source":{"subagent":{"other":"guardian"}}}),
            json!({"id":sid,"parent_thread_id":null,"source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}}}),
            json!({"id":sid,"parent_thread_id":"parent","source":{"subagent":{"thread_spawn":{"parent_thread_id":"unrelated"}}}}),
            json!({"id":"wrong","parent_thread_id":"parent","source":{"subagent":{"other":"guardian"}}}),
        ] {
            std::fs::write(&path, json!({"type":"session_meta","payload":payload}).to_string()+"\n").unwrap();
            let event = json!({"hook_event_name":"SessionStart","session_id":sid,"transcript_path":path});
            let mut record = parent.clone();
            assert!(route(&mut record, &event).is_none());
            assert_eq!(record, parent);
            assert!(!main_start(&record, &event));
            record["subagents"][&sid] = json!({"name":"Reviewer"});
            assert!(route(&mut record, &event).is_none());
            record["conversation_id"] = Value::Null;
            assert!(route(&mut record, &event).is_none());
            assert!(!main_start(&record, &event));
        }
        // A known child's start stays child-scoped even without its rollout.
        let mut record = parent.clone();
        record["subagents"][&sid] = json!({"name":"Reviewer"});
        assert!(!main_start(&record, &json!({"session_id":sid})));
        assert_eq!(route(&mut record, &json!({"hook_event_name":"SessionStart","session_id":sid})).unwrap()["agent_id"], sid);
        // A user fork is an interactive root, despite its parent/fork metadata.
        for source in ["cli", "vscode"] {
            let meta = json!({"type":"session_meta","payload":{"id":sid,"parent_thread_id":"parent","forked_from_id":"parent","source":source}});
            std::fs::write(&path, meta.to_string()+"\n").unwrap();
            let event = json!({"hook_event_name":"SessionStart","session_id":sid,"transcript_path":path});
            assert!(route(&mut parent.clone(), &event).is_none());
            assert!(main_start(&parent, &event));
        }
    }
    #[test]
    fn mixed_children_keep_parent_binding_model_and_history() {
        for (parent, child, sid, path) in [
            (
                "kimi",
                "codex",
                "11111111-1111-4111-8111-111111111111",
                "/tmp/rollout-11111111-1111-4111-8111-111111111111.jsonl",
            ),
            (
                "codex",
                "kimi",
                "session_22222222-2222-4222-8222-222222222222",
                "/tmp/kimi-session",
            ),
        ] {
            let mut record = json!({"agent":parent,"conversation_id":"parent-id","model":"parent-model","transcript":"parent-path","main_done":false,"subagents":{}});
            let mut event = json!({"hook_event_name":"SessionStart","session_id":sid,"transcript_path":path,"cwd":"/tmp/test","model":"child-model"});
            let scoped = route(&mut record, &event).unwrap();
            let id = string(&scoped, "agent_id").to_owned();
            journal::update_activity(&mut record, &scoped);
            assert_eq!(record["conversation_id"], "parent-id");
            assert_eq!(record["transcript"], "parent-path");
            assert_eq!(record["model"], "parent-model");
            assert_eq!(record["subagents"][&id]["provider"], child);
            event["hook_event_name"] = json!("Stop");
            event["last_assistant_message"] = json!("child done");
            let scoped = route(&mut record, &event).unwrap();
            journal::update_activity(&mut record, &scoped);
            assert_eq!(record["subagents"][&id]["state"], "finished");
            assert_eq!(record["subagents"][&id]["detail"], "child done");
            assert_eq!(record["model"], "parent-model");
            assert_eq!(record["main_done"], false);
        }
    }
    #[test]
    fn same_provider_new_conversation_and_unknown_child_are_not_routed() {
        let mut record = json!({"agent":"codex","conversation_id":"parent","subagents":{}});
        assert!(route(&mut record,&json!({"hook_event_name":"SessionStart","session_id":"new","transcript_path":"/tmp/rollout-new.jsonl"})).is_none());
        assert!(route(
            &mut record,
            &json!({"hook_event_name":"Stop","session_id":"unknown"})
        )
        .is_none());
        assert!(route(
            &mut record,
            &json!({"hook_event_name":"SessionStart","session_id":"session_../../other"})
        )
        .is_none());
        assert_eq!(record["subagents"], json!({}));
    }
}
