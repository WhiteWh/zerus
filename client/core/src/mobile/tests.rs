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
fn the_workspace_status_reads_as_qt_checks_it() {
    let workspace = status(code(0, &active().to_string(), "")).unwrap();
    assert_eq!(
        workspace.summary,
        "Example team on workstation\nPrivate session content is encrypted."
    );
    assert_eq!(workspace.connection, "Connector: running");
    assert!(workspace.encrypted && workspace.running && workspace.pending.is_none());
    let unconfirmed = "Workspace operation was not confirmed. Refresh status before continuing. Verify that the matching computer connector is installed.";
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
    assert_eq!(refused(&|v| v["ok"] = json!(false)), unconfirmed);
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
        unconfirmed
    );
    assert_eq!(status(Some(Exit::TimedOut)).unwrap_err(), unconfirmed);
    assert_eq!(status(None).unwrap_err(), unconfirmed);
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
            shell_word("@mac")
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
    assert_eq!(powershell_word("it's mine"), "'it''s mine'");
    assert_eq!(powershell_word("@mac"), "'@mac'");
    assert_eq!(powershell_word("a,b"), "'a,b'");
    assert_eq!(
        powershell_word("https://relay.example.com"),
        "https://relay.example.com"
    );
    assert_eq!(
        pinned("", "revoke-phone", "it's mine", "", ""),
        format!("hgs mobile revoke-phone {} --json", shell_word("it's mine"))
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
    fn program(&self, name: &str, cases: &[(&str, &str)]) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let mut script = format!(
            "#!/bin/sh\nhere=$(dirname \"$0\")\necho \"$* tmux=${{TMUX:-unset}} pane=${{TMUX_PANE:-unset}} session=${{HGS_SESSION:-unset}} run=${{HGS_RUN_ID:-unset}}\" >> \"$here/{name}.calls\"\ncat >/dev/null\ncase \"$*\" in\n"
        );
        for (pattern, answer) in cases {
            script.push_str(&format!("  {pattern}) {answer};;\n"));
        }
        script.push_str("  *) echo unexpected >&2; exit 9;;\nesac\n");
        let path = self.0.join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
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
fn the_connector_is_only_asked_for_read_only_status_and_capability() {
    let scratch = Scratch::new("connector");
    let reply = active().to_string().replace('"', "\\\"");
    let connector = scratch.program(
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
fn phones_are_read_after_the_help_says_the_command_exists() {
    let scratch = Scratch::new("phones");
    let machines = r#"{"machines":[{"alias":"mac","enabled":true},{"alias":"off","enabled":false},{"alias":"","enabled":true}]}"#;
    let listed = json!({"configured": true, "encrypted": true, "server_url": "https://relay.example.com", "workspace_id": "ws-1",
        "workspace_name": "Example team", "devices": [{"id": "p1", "name": "Pixel", "platform": "android", "paired_at": 1_800_000_000,
        "last_seen_at": null, "added_by": {"name": "workstation"}}]}).to_string().replace('"', "\\\"");
    let hgs = scratch.program(
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
    let long = scratch.program(
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
