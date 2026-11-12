// The Mobile connection page's reads (WorkspaceSettings.h, PhonePairing.cpp,
// MobileDeliverySettings.h), against fake programs only: nothing here runs
// the real connector, its service or a gateway.
use super::*;
use serde_json::json;

fn code(code: i32, stdout: &str, stderr: &str) -> Option<Exit> {
    Some(Exit::Code {
        code,
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.into(),
    })
}

fn active() -> Value {
    json!({"state": "active", "encrypted": true, "config_revision": "a".repeat(64), "server_url": "https://relay.example.com",
        "node_id": "node-1", "workspace_id": "ws-1", "workspace_name": "Example team", "computer_name": "workstation",
        "connector_state": "running", "running": true, "generation": "b".repeat(32)})
}

#[test]
fn workspace_failures_distinguish_startup_protocol_and_refusal_without_child_text() {
    let private = "private-child-text-and-credential";
    let cases = [
        (Some(Exit::StartFailed(private.into())), "could not start"),
        (Some(Exit::TimedOut), "timed out"),
        (Some(Exit::WriteFailed), "input could not be delivered"),
        (
            Some(Exit::Abnormal {
                stderr: private.into(),
            }),
            "ended abnormally",
        ),
        (None, "exceeded its limit"),
        (code(-1073741515, private, private), "0xc0000135"),
        (code(0, "[]", private), "invalid workspace reply"),
        (code(0, "{}", private), "status is incompatible"),
        (
            code(1, r#"{"ok":false}"#, private),
            "refused the workspace request",
        ),
    ];
    for (exit, reason) in cases {
        let error = status(exit).unwrap_err();
        assert!(error.contains(reason), "{error}");
        assert!(!error.contains(private));
        assert!(error.len() < 512);
    }
    let error = workspace_reply(
        code(1, &json!({"error":private}).to_string(), private),
        false,
    )
    .unwrap_err();
    assert!(
        !error.contains(private),
        "effect refusals never echo child text"
    );
    assert!(error.contains("refused the workspace request"));
}

/// zerus-7rkq9: only the connector's closed result bound to this very call means
/// that nothing changed. Its error text alone never does.
#[test]
fn only_the_closed_refusal_of_this_call_says_nothing_changed() {
    // The connector's contract, in another package: its tests pin the same words.
    assert_eq!(STOP_REFUSED_BEFORE_EFFECT, "stop-refused-before-effect");
    let (revision, generation) = ("r".repeat(64), "g".repeat(32));
    let refusal = json!({"ok":false,"refusal":STOP_REFUSED_BEFORE_EFFECT,"generation":generation,"expected_revision":revision,"error":"anything"});
    let check = |value: &Value, status: i32| {
        stop_refused(
            &code(status, &value.to_string(), ""),
            &revision,
            &generation,
        )
    };
    assert!(check(&refusal, 1));
    for field in ["ok", "refusal", "generation", "expected_revision"] {
        let mut changed = refusal.clone();
        changed.as_object_mut().unwrap().remove(field);
        assert!(!check(&changed, 1), "without {field}");
        changed[field] = json!("other");
        assert!(!check(&changed, 1), "with another {field}");
    }
    assert!(!check(&refusal, 0), "a clean exit is no refusal");
    assert!(
        !check(&json!({"ok":false,"error":STOP_REFUSED}), 1),
        "text alone"
    );
    assert!(
        !check(&json!({"ok":false,"error":STOP_REFUSED_BEFORE_EFFECT}), 1),
        "text alone"
    );
    assert!(!stop_refused(&Some(Exit::TimedOut), &revision, &generation));
    assert!(!stop_refused(&None, &revision, &generation));
    assert!(!stop_refused(
        &code(1, "not json", ""),
        &revision,
        &generation
    ));
    // workspace_reply itself never reads any of this as anything but a failure.
    let error = workspace_reply(code(1, &refusal.to_string(), ""), false).unwrap_err();
    assert_ne!(error, STOP_REFUSED);
}

#[test]
fn a_missing_selected_connector_is_reported_before_the_status_probe() {
    let missing = std::env::temp_dir().join(format!("missing-connector-{}", uuid::Uuid::new_v4()));
    let error = read_status(&missing, &missing.with_extension("json")).unwrap_err();
    assert!(error.contains("connector is missing"));
    assert!(!error.contains(&missing.to_string_lossy().to_string()));
}

#[test]
fn the_workspace_status_reads_as_qt_checks_it() {
    let workspace = status(code(0, &active().to_string(), "")).unwrap();
    assert_eq!(
        workspace.summary,
        "Example team on workstation\nPrivate session content is encrypted."
    );
    assert_eq!(workspace.connection, "Connector: running");
    assert!(workspace.encrypted && workspace.running && workspace.pending.is_none());
    let unconfirmed = workspace_failure("the computer connector workspace status is incompatible; update the matching Zerus package");
    let refused = |change: &dyn Fn(&mut Value)| {
        let mut value = active();
        change(&mut value);
        status(code(0, &value.to_string(), "")).unwrap_err()
    };
    assert_eq!(refused(&|v| v["generation"] = json!("short")), unconfirmed);
    assert_eq!(
        refused(&|v| v["server_url"] = json!("http://relay.example.com")),
        unconfirmed
    );
    assert_eq!(
        refused(&|v| v["config_revision"] = json!("A".repeat(64))),
        unconfirmed
    );
    assert_eq!(
        refused(&|v| v["ok"] = json!(false)),
        workspace_failure("the computer connector refused the workspace request (exit 0)")
    );
    // As in Qt, a well-formed status is read even with an error field.
    let mut noted = active();
    noted["error"] = json!("note");
    assert!(status(code(0, &noted.to_string(), "")).is_ok());
    // A failed command with a short error of its own says it.
    assert_eq!(
        status(code(
            1,
            r#"{"error":"Connector configuration is unreadable"}"#,
            ""
        ))
        .unwrap_err(),
        "Connector configuration is unreadable"
    );
    assert_eq!(
        status(code(1, &json!({"error": "x".repeat(1025)}).to_string(), "")).unwrap_err(),
        workspace_failure("the computer connector refused the workspace request (exit 1)")
    );
    assert_eq!(
        status(Some(Exit::TimedOut)).unwrap_err(),
        workspace_failure("the computer connector timed out")
    );
    assert_eq!(
        status(None).unwrap_err(),
        workspace_failure("the computer connector reply exceeded its limit")
    );
    // Stopped, legacy, retired, unconfigured and pending states.
    let mut stopped = active();
    stopped["connector_state"] = json!("stopped");
    stopped["running"] = json!(false);
    stopped["generation"] = Value::Null;
    assert_eq!(
        status(code(0, &stopped.to_string(), ""))
            .unwrap()
            .connection,
        "Connector: stopped"
    );
    let legacy = json!({"state": "legacy", "encrypted": false, "config_revision": "", "server_url": "https://relay.example.com"});
    assert_eq!(
        status(code(0, &legacy.to_string(), "")).unwrap().summary,
        "Legacy connection: session content is visible to this relay."
    );
    assert!(status(code(
        0,
        &json!({"state": "legacy", "encrypted": true, "config_revision": ""}).to_string(),
        ""
    ))
    .is_err());
    let none = json!({"state": "unconfigured", "encrypted": false, "config_revision": ""});
    assert_eq!(
        status(code(0, &none.to_string(), "")).unwrap().summary,
        "No workspace connected"
    );
    let mut pending = legacy.clone();
    pending["pending"] = json!({"state": "migration-draining"});
    pending["migration"] =
        json!({"phase": "draining", "retryable": true, "reason": "drain-timeout"});
    let read = status(code(0, &pending.to_string(), "")).unwrap();
    assert_eq!(
        read.pending.as_deref(),
        Some("A workspace connection is saved and waiting. Check its result before starting another connection.")
    );
    pending["migration"]["reason"] = json!("anything");
    assert!(status(code(0, &pending.to_string(), "")).is_err());
    let mut retired = stopped.clone();
    retired["state"] = json!("retired");
    assert_eq!(
        status(code(0, &retired.to_string(), "")).unwrap().summary,
        "This computer was removed from its workspace."
    );
}

#[test]
fn the_workspace_actions_shown_follow_qt_controls() {
    let shown = |value: Value| status(code(0, &value.to_string(), "")).unwrap().controls;
    assert_eq!(
        shown(active()),
        Controls {
            change: true,
            enrollment: false,
            start: true,
            stop: true,
            check: None,
            invitations: true,
        }
    );
    let none = shown(json!({"state": "unconfigured", "encrypted": false, "config_revision": ""}));
    assert_eq!(
        none,
        Controls {
            enrollment: true,
            ..Controls::default()
        }
    );
    let mut legacy = json!({"state": "legacy", "encrypted": false, "config_revision": "", "server_url": "https://relay.example.com"});
    assert_eq!(
        shown(legacy.clone()),
        Controls {
            change: true,
            ..Controls::default()
        }
    );
    // A saved connection: its action is named by its state.
    legacy["pending"] = json!({"state": "pending"});
    assert_eq!(
        shown(legacy.clone()).check.as_deref(),
        Some("Check connection")
    );
    legacy["pending"] = json!({"state": "legacy-retirement-unconfirmed"});
    assert_eq!(
        shown(legacy.clone()).check.as_deref(),
        Some("Disconnect legacy locally and switch…")
    );
    legacy["pending"] = json!({"state": "migration-draining"});
    legacy["migration"] = json!({"phase": "draining", "retryable": true, "reason": "draining"});
    assert_eq!(shown(legacy).check.as_deref(), Some("Continue connecting"));
    // A retired computer shows Stop only while its connector drains.
    let mut retired = active();
    retired["state"] = json!("retired");
    let draining = shown(retired.clone());
    assert!(draining.stop && !draining.start && draining.change && !draining.invitations);
    retired["running"] = json!(false);
    retired["connector_state"] = json!("stopped");
    retired["generation"] = Value::Null;
    assert!(!shown(retired).stop);
    let read = status(code(0, &active().to_string(), "")).unwrap();
    assert_eq!(read.config_revision, "a".repeat(64));
}

#[test]
fn pair_and_revoke_name_the_pinned_commands_qt_runs() {
    assert_eq!(
        pinned("", "invite", "", "ws-1", "https://relay.example.com"),
        "hgs mobile invite --workspace ws-1 --server https://relay.example.com --json"
    );
    assert_eq!(
        pinned("mac", "revoke-phone", "p1", "ws-1", "https://relay.example.com"),
        format!(
            "hgs {} mobile revoke-phone p1 --workspace ws-1 --server https://relay.example.com --json",
            shell_word("@mac").unwrap()
        )
    );
    // Without both pins Qt sends neither.
    assert_eq!(
        pinned("", "invite", "", "ws-1", ""),
        "hgs mobile invite --json"
    );
    // Odd words are quoted for the terminal they are typed into: a POSIX
    // shell, or PowerShell on Windows, where @ splats and , makes an array.
    assert_eq!(posix_word("it's mine"), "'it'\\''s mine'");
    assert_eq!(posix_word("@mac"), "@mac");
    // zerus-sa9qn.43: the POSIX word is shell::shell_quote, whole in fish too.
    for word in ["x\\'; touch pwned; #", "@mac\\'", "a\\b", "it's"] {
        assert_eq!(posix_word(word), crate::shell::shell_quote(word));
        assert_eq!(
            crate::shell::fish_words(&posix_word(word)),
            Ok(vec![word.to_owned()])
        );
    }
    assert_eq!(
        powershell_word("it's mine").as_deref(),
        Some("'it''s mine'")
    );
    assert_eq!(powershell_word("@mac").as_deref(), Some("'@mac'"));
    assert_eq!(powershell_word("a,b").as_deref(), Some("'a,b'"));
    // zerus-sa9qn.38: PowerShell also closes a single-quoted string on the
    // quotes U+2018 to U+201B, so each doubles like `'`; an alias such as
    // `mac\u{2019};calc;\u{2019}` stays one word (fleet::valid_alias accepts it).
    for quote in ['\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}'] {
        assert_eq!(
            powershell_word(&format!("@mac{quote};calc;{quote}")),
            Some(format!("'@mac{quote}{quote};calc;{quote}{quote}'"))
        );
    }
    assert_eq!(
        powershell_word("it's \u{2019}$(calc)`").as_deref(),
        Some("'it''s \u{2019}\u{2019}$(calc)`'")
    );
    // zerus-sa9qn.43: Windows PowerShell 5.1 hands hgs.exe a spaced argument
    // in "…" without escaping the " inside, so `ws" --server "evil" "x` would
    // pin another relay; such a word, and a spaced one ending in a
    // backslash, gets no command.
    for unsafe_word in [
        "ws\" --server \"https://evil.example\" \"x",
        "a\"b",
        "@mac\u{201c}x",
        "a\u{201d}b",
        "a\u{201e}b",
        "a b\\",
    ] {
        assert_eq!(powershell_word(unsafe_word), None, "{unsafe_word:?}");
    }
    assert_eq!(powershell_word("a\\b").as_deref(), Some("'a\\b'"));
    if cfg!(windows) {
        assert_eq!(
            pinned(
                "",
                "invite",
                "",
                "ws\" --server \"x",
                "https://relay.example.com"
            ),
            ""
        );
    }
    assert_eq!(
        powershell_word("https://relay.example.com").as_deref(),
        Some("https://relay.example.com")
    );
    assert_eq!(
        pinned("", "revoke-phone", "it's mine", "", ""),
        format!(
            "hgs mobile revoke-phone {} --json",
            shell_word("it's mine").unwrap()
        )
    );
}

#[test]
fn relay_addresses_are_checked_as_qt_checks_them() {
    assert!(valid_relay("https://relay.example.net"));
    assert!(valid_relay("https://relay.example.com:8443/base"));
    for bad in [
        "",
        "http://relay.example.com",
        "https://",
        "https://name@example.com",
        "https://relay.example.com/?q=1",
        "https://relay.example.com/#x",
        "https://relay example.com",
        "https://:443",
    ] {
        assert!(!valid_relay(bad), "{bad}");
    }
}

#[test]
fn phones_list_one_workspace_once_this_computer_first() {
    let phone = |id: &str| Phone {
        id: id.into(),
        name: format!("Phone {id}"),
        platform: "iOS".into(),
        paired_at: Some(1_800_000_000.0),
        last_seen_at: None,
        added_from: None,
        revoke: String::new(),
    };
    let list = |host: &str, workspace: &str, phones: Vec<Phone>| PhoneList {
        host: host.into(),
        workspace_id: workspace.into(),
        workspace_name: format!("Team {workspace}"),
        encrypted: true,
        server_url: "https://relay.example.com".into(),
        invite: String::new(),
        phones,
    };
    let view = phones_view(
        vec![
            (
                String::new(),
                Ok(Some(list("", "ws-1", vec![phone("a"), phone("b")]))),
            ),
            (
                "mac".into(),
                Ok(Some(list("mac", "ws-1", vec![phone("a")]))),
            ),
            (
                "lab".into(),
                Ok(Some(list("lab", "ws-2", vec![phone("c")]))),
            ),
            (
                "old".into(),
                Err("Update Zerus on old to manage phones from here.".into()),
            ),
            ("none".into(), Ok(None)),
        ],
        None,
        false,
    );
    let routes: Vec<&str> = view.lists.iter().map(|l| l.host.as_str()).collect();
    assert_eq!(routes, ["", "lab"]);
    assert_eq!(
        view.lines,
        [
            "3 phones",
            "old: Update Zerus on old to manage phones from here."
        ]
    );
    let empty = phones_view(vec![], Some("timed out".into()), false);
    assert_eq!(
        empty.lines,
        [
            "No computer here has a mobile connection. Set one up in Settings → Mobile connection on a computer with local sessions enabled, then refresh Phones.",
            "Could not read the machine list: timed out",
        ]
    );
    let one = phones_view(
        vec![(String::new(), Ok(Some(list("", "ws-1", vec![phone("a")]))))],
        None,
        true,
    );
    assert_eq!(one.lines, ["1 phone"]);
}

/// A folder of the test's own, removed however the test ends.
#[cfg(unix)]
struct Scratch(std::path::PathBuf);

#[cfg(unix)]
impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("zerus-mobile-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// A fake program logging its arguments (and whether TMUX or the agent's
    /// attribution reached it) and answering per the first matching case.
    fn program(
        &self,
        name: &str,
        cases: &[(&str, &str)],
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let mut script = format!(
            "#!/bin/sh\nhere=$(dirname \"$0\")\necho \"$* tmux=${{TMUX:-unset}} pane=${{TMUX_PANE:-unset}} session=${{HGS_SESSION:-unset}} run=${{HGS_RUN_ID:-unset}}\" >> \"$here/{name}.calls\"\ncat >/dev/null\ncase \"$*\" in\n"
        );
        for (pattern, answer) in cases {
            script.push_str(&format!("  {pattern}) {answer};;\n"));
        }
        script.push_str("  *) echo unexpected >&2; exit 9;;\nesac\n");
        let path = self.0.join(name);
        let payload = self.0.join(format!("{name}.fixture"));
        // A parallel fork can retain a new executable's write fd until exec,
        // making this test's exec fail with ETXTBSY after our writer closed.
        // Execute only the checked-in launcher; the completed payload is read
        // as shell input and is never executable.
        std::fs::write(&payload, script).unwrap();
        std::os::unix::fs::symlink(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_mobile.sh"),
            &path,
        )
        .unwrap();
        (path, payload)
    }

    fn calls(&self, name: &str) -> String {
        std::fs::read_to_string(self.0.join(format!("{name}.calls"))).unwrap_or_default()
    }
}

#[cfg(unix)]
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
#[test]
fn a_staged_phone_cli_receives_the_exact_connector_without_global_environment_changes() {
    let bundle = Scratch::new("phone-bundle");
    let stage = Scratch::new("phone-stage");
    let (connector, _) = bundle.program(
        "zerus-mobile-connector",
        &[("*", "echo '{\"nested\":true}'")],
    );
    let (stale, _) = stage.program("zerus-mobile-connector", &[("*", "echo stale; exit 9")]);
    let (hgs, _) = stage.program("hgs", &[("*", "if [ -n \"${ZERUS_CLIENT_MOBILE_CONNECTOR_PATH+x}\" ]; then \"$ZERUS_CLIENT_MOBILE_CONNECTOR_PATH\" --workspace status; else \"$here/zerus-mobile-connector\" --workspace status; fi")]);
    let call = invocation(&["mobile", "phones", "--json"], Duration::from_secs(5));
    // Existing fallback really reaches the competing sibling.
    assert!(matches!(
        run_phone_cli_in(&hgs, &call, 4096, None).unwrap(),
        Exit::Code { code: 9, .. }
    ));
    let old_calls = stage.calls("zerus-mobile-connector");
    let result = run_phone_cli_in(&hgs, &call, 4096, connector.to_str()).unwrap();
    assert!(
        matches!(result, Exit::Code { code: 0, ref stdout, .. } if stdout == b"{\"nested\":true}\n")
    );
    assert_eq!(
        stage.calls("zerus-mobile-connector"),
        old_calls,
        "exact selection must bypass the stale sibling"
    );
    assert!(bundle
        .calls("zerus-mobile-connector")
        .starts_with("--workspace status "));
    assert!(stale.exists());
    let remote = invocation(
        &["@lab", "mobile", "phones", "--json"],
        Duration::from_secs(5),
    );
    assert!(phone_connector(&hgs, &remote, Some(&bundle.0)).is_none());
}

/// zerus-ihrzs: on every platform a local nested phone call carries the
/// connector the status probe selected. With no connector beside a separately
/// staged hgs, the probe picks the one packaged beside the app; the nested CLI
/// must not fall back to a different connector found on PATH.
#[cfg(unix)]
#[test]
fn a_local_phone_call_carries_the_probe_selection_when_no_sibling_exists() {
    let app = Scratch::new("phone-app");
    let stage = Scratch::new("phone-lone-stage");
    let on_path = Scratch::new("phone-path");
    let (packaged, _) = app.program(
        "zerus-mobile-connector",
        &[("*", "echo '{\"packaged\":true}'")],
    );
    on_path.program("zerus-mobile-connector", &[("*", "echo stale; exit 9")]);
    // Mirrors src/phones.rs: the exact selection, else the sibling, else PATH.
    let fallback = format!(
        "if [ -n \"${{ZERUS_CLIENT_MOBILE_CONNECTOR_PATH+x}}\" ]; then \"$ZERUS_CLIENT_MOBILE_CONNECTOR_PATH\" --workspace status; elif [ -x \"$here/zerus-mobile-connector\" ]; then \"$here/zerus-mobile-connector\" --workspace status; else PATH='{}':\"$PATH\" zerus-mobile-connector --workspace status; fi",
        on_path.0.display()
    );
    let (hgs, _) = stage.program("hgs", &[("*", fallback.as_str())]);
    assert_eq!(connector_program(&hgs, Some(&app.0)), packaged);
    let call = invocation(&["mobile", "phones", "--json"], Duration::from_secs(5));
    let result = run_phone_cli_from(&hgs, &call, 4096, Some(&app.0)).unwrap();
    assert!(
        matches!(result, Exit::Code { code: 0, ref stdout, .. } if stdout == b"{\"packaged\":true}\n"),
        "{result:?}"
    );
    assert_eq!(
        on_path.calls("zerus-mobile-connector"),
        "",
        "the stale PATH connector was reached"
    );
    // A remote call still carries no local selection.
    let remote = invocation(
        &["@lab", "mobile", "phones", "--json"],
        Duration::from_secs(5),
    );
    assert!(phone_connector(&hgs, &remote, Some(&app.0)).is_none());
}

#[cfg(windows)]
#[test]
fn the_packaged_windows_connector_wins_over_a_legacy_cli_sibling() {
    struct Owned(std::path::PathBuf);
    impl Drop for Owned {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let root =
        std::env::temp_dir().join(format!("zerus-mobile-selection-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let root = Owned(root);
    let app = root.0.join("app");
    let stage = root.0.join("stage");
    std::fs::create_dir(&app).unwrap();
    std::fs::create_dir(&stage).unwrap();
    let name = "zerus-mobile-connector.exe";
    let current = app.join(name);
    std::fs::write(&current, b"current package fixture").unwrap();
    std::fs::write(stage.join(name), b"legacy fixture").unwrap();
    let hgs = stage.join("hgs.exe");
    assert_eq!(connector_program(&hgs, Some(&app)), current);
    let call = invocation(&["mobile", "phones", "--json"], Duration::from_secs(5));
    let selected = phone_connector(&hgs, &call, Some(&app)).unwrap();
    assert_eq!(Path::new(&selected), current);
    let remote = invocation(
        &["@lab", "mobile", "phones", "--json"],
        Duration::from_secs(5),
    );
    assert!(phone_connector(&hgs, &remote, Some(&app)).is_none());
}

/// The real call path against a connector that advertises Stop and refuses it.
#[cfg(unix)]
fn running_reply() -> Map<String, Value> {
    let mut value = active();
    value["running"] = json!(true);
    value["connector_state"] = json!("running");
    value["generation"] = json!("e".repeat(32));
    value.as_object().unwrap().clone()
}

#[cfg(unix)]
fn running_status(reply: &Map<String, Value>) -> String {
    Value::Object(reply.clone())
        .to_string()
        .replace('"', "\\\"")
}

#[cfg(unix)]
#[test]
fn a_stop_refused_by_the_connector_reaches_the_caller_as_the_closed_result() {
    let scratch = Scratch::new("stop-refused");
    let running = running_reply();
    let reply = running_status(&running);
    let revision = text(&running, "config_revision");
    let generation = text(&running, "generation");
    let refusal = json!({"ok":false,"refusal":STOP_REFUSED_BEFORE_EFFECT,"generation":generation,"expected_revision":revision}).to_string().replace('"', "\\\"");
    let (connector, _) = scratch.program(
        "zerus-mobile-connector",
        &[
            ("*--workspace\\ status*", &format!("echo \"{reply}\"")),
            (
                "*--workspace\\ stop*",
                &format!("echo \"{refusal}\"; exit 1"),
            ),
            ("--help", &format!("echo '{STOP_MARKER}'")),
        ],
    );
    let config = scratch.0.join("connector.json");
    let current = read_status(&connector, &config).unwrap();
    let target = connector_actions::admit(&current, connector_actions::Kind::Stop).unwrap();
    let failure =
        connector_actions::apply(&connector, &config, &target, connector_actions::Kind::Stop)
            .err()
            .unwrap();
    assert_eq!(failure, Failure::StopRefused);
    assert_eq!(failure.message(), STOP_REFUSED);
}

#[cfg(unix)]
#[test]
fn a_workspace_change_whose_stop_is_refused_changes_nothing_and_says_so() {
    let scratch = Scratch::new("change-refused");
    let running = running_reply();
    let reply = running_status(&running);
    let revision = text(&running, "config_revision");
    let generation = text(&running, "generation");
    let refusal = json!({"ok":false,"refusal":STOP_REFUSED_BEFORE_EFFECT,"generation":generation,"expected_revision":revision}).to_string().replace('"', "\\\"");
    let (connector, _) = scratch.program(
        "zerus-mobile-connector",
        &[
            ("*--workspace\\ status*", &format!("echo \"{reply}\"")),
            (
                "*--workspace\\ stop*",
                &format!("echo \"{refusal}\"; exit 1"),
            ),
            ("--help", &format!("echo '{STOP_MARKER}'")),
        ],
    );
    let config = scratch.0.join("connector.json");
    let current = read_status(&connector, &config).unwrap();
    let target = enrollment::Target::capture(&current).unwrap();
    let request = enrollment::Request {
        mode: enrollment::Mode::Create,
        relay: "https://relay.example.invalid".into(),
        code: String::new(),
    };
    let failure = enrollment::enroll(&connector, &config, &target, &request, "Computer")
        .err()
        .unwrap();
    assert_eq!(failure, Failure::StopRefused);
    let calls = scratch.calls("zerus-mobile-connector");
    assert!(calls.contains("--workspace stop"), "{calls}");
    assert!(!calls.contains("--workspace switch"), "{calls}");
}

/// A connector that doesn't advertise Stop never receives one (zerus-7rkq9).
#[cfg(unix)]
#[test]
fn a_connector_without_the_stop_marker_is_never_sent_a_stop_or_a_running_change() {
    let scratch = Scratch::new("stop-unadvertised");
    let running = running_reply();
    let reply = running_status(&running);
    let (connector, _) = scratch.program(
        "zerus-mobile-connector",
        &[
            ("*--workspace\\ status*", &format!("echo \"{reply}\"")),
            ("--help", "echo 'no marker here'"),
        ],
    );
    let config = scratch.0.join("connector.json");
    let current = read_status(&connector, &config).unwrap();
    assert!(!current.stop_supported);
    assert!(connector_actions::admit(&current, connector_actions::Kind::Stop).is_err());
    let target = enrollment::Target::capture(&current).unwrap();
    assert!(!target.can_enroll(), "a running change needs Stop");
    let mut advertised = target.clone();
    advertised.stop_supported = true;
    assert!(connector_actions::apply(
        &connector,
        &config,
        &advertised,
        connector_actions::Kind::Stop
    )
    .is_err());
    assert!(
        !scratch
            .calls("zerus-mobile-connector")
            .contains("--workspace stop"),
        "a Stop was sent"
    );
}

#[cfg(unix)]
#[test]
fn the_connector_is_only_asked_for_read_only_status_and_capability() {
    let scratch = Scratch::new("connector");
    let reply = active().to_string().replace('"', "\\\"");
    let (connector, _) = scratch.program(
        "zerus-mobile-connector",
        &[
            ("*--workspace\\ status*", &format!("echo \"{reply}\"")),
            ("--help", &format!("echo '{STOP_MARKER}'")),
        ],
    );
    let config = scratch.0.join("connector.json");
    // As if this client ran in a terminal of an agent's session.
    let workspace = read_status_in(
        &connector,
        &config,
        &[
            ("TMUX", "/tmp/tmux-1000/default,1,0"),
            ("TMUX_PANE", "%1"),
            ("HGS_SESSION", "claude/sample/agent"),
            ("HGS_RUN_ID", "run-1"),
        ],
    )
    .unwrap();
    assert_eq!(workspace.connection, "Connector: running");
    let calls = scratch.calls("zerus-mobile-connector");
    assert!(workspace.stop_supported);
    let scrubbed = "tmux=unset pane=unset session=unset run=unset";
    assert_eq!(
        calls.lines().map(str::to_owned).collect::<Vec<_>>(),
        [
            format!(
                "--workspace status --config {} {scrubbed}",
                config.display()
            ),
            format!("--help {scrubbed}"),
        ],
        "Only these two read-only calls may run, with attribution scrubbed on each"
    );
    // Found beside an absolute hgs before anything else.
    let hgs = scratch.0.join("hgs");
    std::fs::write(&hgs, "").unwrap();
    assert_eq!(connector_program(&hgs, None), connector);
}

#[cfg(unix)]
#[test]
fn a_held_fixture_writer_does_not_block_the_status_read() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = Scratch::new("held-writer");
    let reply = active().to_string().replace('"', "\\\"");
    let (connector, payload) = scratch.program(
        "zerus-mobile-connector",
        &[
            ("*--workspace\\ status*", &format!("echo \"{reply}\"")),
            ("--help", &format!("echo '{STOP_MARKER}'")),
        ],
    );
    // Model the writable descriptor a concurrent child inherited during
    // fixture publication. Holding it must not prevent either read-only call.
    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(&payload)
        .unwrap();
    let workspace = read_status(&connector, &scratch.0.join("connector.json")).unwrap();
    assert_eq!(workspace.connection, "Connector: running");
    assert!(workspace.stop_supported);
    assert_eq!(scratch.calls("zerus-mobile-connector").lines().count(), 2);
    assert_eq!(held.metadata().unwrap().permissions().mode() & 0o111, 0);
}

#[cfg(unix)]
#[test]
fn phones_are_read_after_the_help_says_the_command_exists() {
    let scratch = Scratch::new("phones");
    let machines = r#"{"machines":[{"alias":"mac","enabled":true},{"alias":"off","enabled":false},{"alias":"","enabled":true}]}"#;
    let listed = json!({"configured": true, "encrypted": true, "server_url": "https://relay.example.com", "workspace_id": "ws-1",
        "workspace_name": "Example team", "devices": [{"id": "p1", "name": "Pixel", "platform": "android", "paired_at": 1_800_000_000,
        "last_seen_at": null, "added_by": {"name": "workstation"}}]}).to_string().replace('"', "\\\"");
    let (hgs, _) = scratch.program(
        "hgs",
        &[
            ("machine\\ ls", &format!("echo '{machines}'")),
            ("@mac\\ --help", "echo 'hgs [@host] ls'"),
            (
                "--help",
                "echo 'hgs [@host] mobile phones|invite|revoke-phone <id> --json'",
            ),
            ("mobile\\ phones\\ --json", &format!("echo \"{listed}\"")),
            (
                "swarm\\ preference\\ get\\ --json",
                "echo '{\"mobile_delivery\":\"away_5\"}'",
            ),
            (
                "swarm\\ preference\\ set\\ mobile_delivery\\ away_10\\ --json",
                "echo '{\"ok\":true,\"mobile_delivery\":\"away_10\"}'",
            ),
        ],
    );
    assert_eq!(hosts(&hgs).unwrap(), ["", "mac"]);
    let list = phones(&hgs, "").unwrap().unwrap();
    assert_eq!(
        (list.workspace_name.as_str(), list.phones.len()),
        ("Example team", 1)
    );
    assert_eq!(
        list.invite,
        "hgs mobile invite --workspace ws-1 --server https://relay.example.com --json"
    );
    assert_eq!(
        list.phones[0].revoke,
        "hgs mobile revoke-phone p1 --workspace ws-1 --server https://relay.example.com --json"
    );
    let phone = &list.phones[0];
    assert_eq!(
        (
            phone.platform.as_str(),
            phone.added_from.as_deref(),
            phone.last_seen_at
        ),
        ("Android", Some("workstation"), None)
    );
    // An older hgs on a peer is never asked the unknown word.
    assert_eq!(
        phones(&hgs, "mac").unwrap_err(),
        "Update Zerus on mac to manage phones from here."
    );
    let calls = scratch.calls("hgs");
    assert!(!calls.contains("@mac mobile"), "{calls}");
    assert!(
        calls
            .lines()
            .all(|line| line.ends_with("session=unset run=unset")),
        "{calls}"
    );
    // A help past its own limit says so.
    let (long, _) = scratch.program(
        "long",
        &[("--help", "head -c 300000 /dev/zero | tr '\\0' x")],
    );
    assert_eq!(phones(&long, "").unwrap_err(), "reply exceeds 262144 bytes");
    // The shared notification timing: only Qt's values are ever sent.
    assert_eq!(delivery(&hgs, None).unwrap(), "away_5");
    assert_eq!(delivery(&hgs, Some("away_10")).unwrap(), "away_10");
    let before = scratch.calls("hgs").lines().count();
    assert!(delivery(&hgs, Some("sometimes")).is_err());
    assert_eq!(scratch.calls("hgs").lines().count(), before);
}
