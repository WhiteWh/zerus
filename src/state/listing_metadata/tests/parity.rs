use super::*;

#[test]
fn public_terminal_summaries_match_the_canonical_builder() {
    let mut failures = Vec::new();
    for case in [
        "recipe",
        "error",
        "settings",
        "history",
        "quiet",
        "system-prompt",
        "id",
        "children",
        "parent",
        "account",
        "fork-origin",
        "provider-error",
    ] {
        let home = crate::host::staging::private_tempdir();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "state::listing_metadata::tests::parity::isolated_parity",
                "--ignored",
                "--nocapture",
            ])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", home.path())
            .env("HGS_STATE_DIR", home.path().join("state"))
            .env("HGS_CONFIG_DIR", home.path().join("config"))
            .env("ZERUS_LISTING_PARITY", case)
            .output()
            .unwrap();
        if !output.status.success() {
            failures.push(format!(
                "{case}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
#[ignore = "private summary state"]
fn isolated_parity() {
    let Ok(case) = std::env::var("ZERUS_LISTING_PARITY") else {
        return;
    };
    let root = root();
    private_dir(&root).unwrap();
    let project = root.join("project");
    private_dir(&project).unwrap();
    let mut value = record();
    value.as_object_mut().unwrap().remove("shell_jobs");
    value["base"] = json!(["codex"]);
    value["launch_dir"] = json!(project);
    value["model"] = json!("example");
    value["effort"] = json!("medium");
    match case.as_str() {
        "recipe" => {}
        "error" => {
            value["error"] = json!("native fixture failed");
        }
        "settings" => {
            value["pending_settings"] =
                json!({"request_id":"settings-1","model":"next","effort":"high"});
            value["expected_id"] = json!("waiting");
        }
        "history" => {
            let transcript = root.join(format!(
                "rollout-{}.jsonl",
                string(&value, "conversation_id")
            ));
            let header = json!({"type":"session_meta","payload":{"id":value["conversation_id"]}});
            let turn =
                json!({"type":"turn_context","payload":{"model":"example-native","effort":"high"}});
            fs::write(&transcript, format!("{header}\n{turn}\n")).unwrap();
            value["transcript"] = json!(transcript);
        }
        "quiet" => {
            value["prompt"] = json!("automatic keep-warm prompt");
            value["keep_warm_quiet"] =
                json!({"prompt":"Original request","last_message":"Original reply"});
        }
        "system-prompt" => {
            value["agent"] = json!("claude");
            value["name"] = json!("claude/example/one");
            value["prompt"] = json!(format!(
                "<task-notification><task-id>fixture</task-id><summary>Finished</summary><result>{}</result></task-notification>",
                "x".repeat(512)
            ));
        }
        "id" => {
            value["layout"] = json!("id");
            value["launch_label"] = value["name"].clone();
        }
        "children" => {
            value["subagents"] = json!({"child":{"name":"Worker","state":"working","prompt":"Child work","reply_id":"reply-child","started":12}});
        }
        "parent" => {
            let parent = json!({
                "name": "codex/example/parent",
                "session_id": "00000000-0000-4000-8000-000000000002",
                "run_id": "parent-run",
                "conversation_id": "parent-conversation",
            });
            value["parent"] = parent_link::link_of(
                &parent,
                "00000000-0000-4000-8000-000000000001",
                "example",
                "launch",
            )
            .unwrap();
        }
        "account" => {
            value["account_binding"] =
                json!({"id":"fixture-account","agent":"codex","home":root.join("account")});
        }
        "fork-origin" => {
            value["fork_parent_id"] = json!("parent-conversation");
            value["fork_source_name"] = json!("codex/example/source");
        }
        "provider-error" => {
            value["provider_error"] =
                json!({"kind":"authentication","detail":"Native authentication failed","at":9});
        }
        _ => panic!("unknown parity fixture"),
    }
    let selected = if value["layout"] == "id" {
        root.join("sessions")
            .join(format!("{}.json", string(&value, "session_id")))
    } else {
        root.join(format!(
            "{}.json",
            storage::label_key(string(&value, "name"))
        ))
    };
    let key = Key::at(&root, &selected).unwrap();
    let seed = Seed::encode(&key, &value).unwrap();
    let decoded = Seed::decode(&key, &seed).unwrap();
    let expected = journal::summary(&value, false);
    let actual = journal::summary(&decoded["record"], false);
    if case == "parent" {
        assert_eq!(actual["parent"], value["parent"]);
    }
    let fields = actual
        .as_object()
        .unwrap()
        .keys()
        .chain(expected.as_object().unwrap().keys())
        .collect::<std::collections::BTreeSet<_>>();
    let differences = fields
        .into_iter()
        .filter(|key| actual[*key] != expected[*key])
        .map(|key| format!("{key}: expected {}, got {}", expected[key], actual[key]))
        .collect::<Vec<_>>();
    assert!(
        differences.is_empty(),
        "{case}: public metadata changed the canonical summary: {}",
        differences.join("; ")
    );
}

#[test]
fn private_transition_data_is_not_cached_as_a_public_launch_recipe() {
    for marker in [
        "pausing",
        "terminating",
        "original_owner",
        "clear_context_request",
        "restore_archive_cleanup",
    ] {
        let mut value = record();
        value[marker] = json!({"private":"must-never-be-cached"});
        let seed = Seed::encode(
            &Key::at(Path::new(""), &path(Path::new(""))).unwrap(),
            &value,
        )
        .unwrap();
        assert!(!seed.contains("must-never-be-cached"));
        let value: Value = serde_json::from_str(&seed).unwrap();
        assert_eq!(
            value["record"]["_listing"]["requires_canonical"], true,
            "{marker}: private transition needs fresh bounded canonical evidence"
        );
    }
}

#[test]
fn native_bindings_do_not_require_the_terminal_record_schema() {
    for agent in ["dsh", "opencode"] {
        for id_layout in [false, true] {
            let session = uuid::Uuid::new_v4().to_string();
            let name = format!("{agent}/example/native");
            let mut record = json!({"name":name,"agent":agent,"backend":agent,
                "run_id":"native-run","session_id":session,"conversation_id":if agent=="dsh" {"session-native"} else {"ses_native"},
                "agent_home":"/srv/example/home","cwd":"/srv/example/project","created":1});
            if id_layout {
                record["layout"] = json!("id");
            }
            let source = if id_layout {
                format!("{agent}/sessions/{session}.json")
            } else {
                format!("{agent}/bindings/{}.json", storage::label_key(&name))
            };
            let key = Key(source);
            let seed = Seed::encode(&key, &record)
                .expect("native provider bindings are intentionally unversioned");
            let decoded = Seed::decode(&key, &seed).unwrap();
            for field in [
                "name",
                "agent",
                "backend",
                "run_id",
                "session_id",
                "conversation_id",
                "agent_home",
                "cwd",
                "created",
            ] {
                assert_eq!(decoded["record"][field], record[field], "{agent} {field}");
            }
        }
    }
}
