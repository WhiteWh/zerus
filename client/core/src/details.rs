//! The inspect fields of the selected session that enable header actions
//! (`m_details` in the Qt client). Adopted only when they name the row's run
//! and conversation. The followed session's Activity `state` value is the same
//! inspection (`src/state/watch_source.rs`); the frontend reads these fields
//! from it and runs this inspect only when no such value has arrived.
use crate::actions::{prepend_alias, Exit, Invocation, Target};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Details {
    pub run_id: String,
    pub conversation_id: String,
    pub auth_required: bool,
    pub expected_id: String,
    pub resume_message_can_send: bool,
    #[serde(default)]
    pub fork_supported: bool,
    #[serde(default)]
    pub fork_reason: String,
    pub allowed_actions: Option<Vec<String>>,
    pub action_reasons: BTreeMap<String, String>,
    /// Stop is offered only when the host can interrupt this turn
    /// (`interrupt::available`, `src/state/journal.rs`).
    pub interrupt_supported: bool,
    /// The turn an interrupt is pinned to; `hgs interrupt` compares it exactly.
    pub turn_started: f64,
    /// The inspected phase; Stop needs `working`, `tool` or `compacting`.
    pub phase: String,
    /// The session's working directory (the raw `cwd`); None when it is not a
    /// string, while a present empty string stays empty (as Qt reads it).
    pub cwd: Option<String>,
    /// The canonical working directory, None when it is not a string; the
    /// row's form stands in then (`SessionsWindow.cpp:2924`).
    pub cwd_canonical: Option<String>,
    /// `terminal_supported` and `terminal_attach_supported`, None when the
    /// inspection has no boolean.
    pub terminal_supported: Option<bool>,
    pub terminal_attach_supported: Option<bool>,
}

/// `hgs [@host] inspect NAME --after 0 --skip-processes [--archive ID]`, 5 s
/// here and 15 s on a peer (`HgsClient.cpp:925-931`).
pub fn details_invocation(target: &Target, archive_id: Option<&str>) -> Invocation {
    let mut args = vec![
        "inspect".to_owned(),
        target.name.clone(),
        "--after".to_owned(),
        "0".to_owned(),
        "--skip-processes".to_owned(),
    ];
    if let Some(id) = archive_id.filter(|id| !id.is_empty()) {
        args.extend(["--archive".to_owned(), id.to_owned()]);
    }
    let peer = prepend_alias(&mut args, &target.computer);
    Invocation {
        args,
        stdin: None,
        timeout: Duration::from_secs(if peer { 15 } else { 5 }),
        request_id: None,
    }
}

/// One bounded child inspection when it opens. The CLI verifies roster membership.
pub fn subagent_identity_invocation(
    target: &Target,
    archive_id: Option<&str>,
    agent_id: &str,
) -> Option<Invocation> {
    if target.run_id.is_empty()
        || target.conversation_id.is_empty()
        || agent_id.is_empty()
        || agent_id.len() > 160
        || agent_id.contains(['/', '\\'])
        || agent_id.chars().any(char::is_control)
    {
        return None;
    }
    let mut call = details_invocation(target, archive_id);
    call.args.extend(["--subagent".into(), agent_id.into()]);
    Some(call)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubagentIdentity {
    pub agent_id: String,
    pub conversation_id: String,
    /// The child's own directory; never substitute the parent's.
    pub cwd: Option<String>,
}

/// Adopt only the requested child of this exact parent run/conversation.
pub fn parse_subagent_identity(
    target: &Target,
    agent_id: &str,
    exit: Exit,
) -> Option<SubagentIdentity> {
    let Exit::Code {
        code: 0, stdout, ..
    } = exit
    else {
        return None;
    };
    let value: Value = serde_json::from_slice(&stdout).ok()?;
    if text(&value, "name") != target.name
        || text(&value, "run_id") != target.run_id
        || text(&value, "parent_conversation_id") != target.conversation_id
        || text(&value, "agent_id") != agent_id
    {
        return None;
    }
    let conversation_id = text(&value, "conversation_id");
    if conversation_id.is_empty()
        || conversation_id.len() > 1024
        || conversation_id.chars().any(char::is_control)
    {
        return None;
    }
    Some(SubagentIdentity {
        agent_id: agent_id.into(),
        conversation_id,
        cwd: value.get("cwd").and_then(Value::as_str).map(str::to_owned),
    })
}

/// One bounded read-only child Activity page, with the same exact identity
/// checks as the header's identity read. The caller retains no other pages.
pub fn parse_subagent_activity(target: &Target, agent_id: &str, exit: Exit) -> Option<Value> {
    let Exit::Code {
        code: 0,
        stdout,
        stderr,
    } = exit
    else {
        return None;
    };
    parse_subagent_identity(
        target,
        agent_id,
        Exit::Code {
            code: 0,
            stdout: stdout.clone(),
            stderr,
        },
    )?;
    let value: Value = serde_json::from_slice(&stdout).ok()?;
    let events = value.get("events")?.as_array()?;
    if events.len() > 200 || !events.iter().all(Value::is_object) {
        return None;
    }
    Some(value)
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// The details when the call exited 0 and the JSON names `target.run_id` and
/// `target.conversation_id`; otherwise None.
pub fn parse_details(target: &Target, exit: Exit) -> Option<Details> {
    let Exit::Code {
        code: 0, stdout, ..
    } = exit
    else {
        return None;
    };
    let value: Value = serde_json::from_slice(&stdout).ok()?;
    let run_id = text(&value, "run_id");
    let conversation_id = text(&value, "conversation_id");
    if run_id != target.run_id || conversation_id != target.conversation_id {
        return None;
    }
    let allowed_actions = value
        .get("allowed_actions")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        });
    let action_reasons = value
        .get("action_reasons")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(key, reason)| Some((key.clone(), reason.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    Some(Details {
        run_id,
        conversation_id,
        auth_required: flag(&value, "auth_required"),
        expected_id: text(&value, "expected_id"),
        resume_message_can_send: flag(&value, "resume_message_can_send"),
        fork_supported: flag(&value, "fork_supported"),
        fork_reason: text(&value, "fork_reason"),
        allowed_actions,
        action_reasons,
        interrupt_supported: flag(&value, "interrupt_supported"),
        turn_started: value
            .get("turn_started")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        phase: text(&value, "phase"),
        cwd: value.get("cwd").and_then(Value::as_str).map(str::to_owned),
        cwd_canonical: value
            .get("cwd_canonical")
            .and_then(Value::as_str)
            .map(str::to_owned),
        terminal_supported: value.get("terminal_supported").and_then(Value::as_bool),
        terminal_attach_supported: value
            .get("terminal_attach_supported")
            .and_then(Value::as_bool),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_capability_survives_the_bounded_inspection_projection() {
        let target = Target {
            computer: "studio".into(),
            name: "codex/example/review".into(),
            run_id: "run".into(),
            conversation_id: "conversation".into(),
        };
        let parse = |body: &str| {
            parse_details(
                &target,
                Exit::Code {
                    code: 0,
                    stdout: body.as_bytes().to_vec(),
                    stderr: String::new(),
                },
            )
            .unwrap()
        };
        let details = parse(
            r#"{"run_id":"run","conversation_id":"conversation","fork_supported":true,"fork_reason":"Ready to fork"}"#,
        );
        assert!(details.fork_supported);
        assert_eq!(details.fork_reason, "Ready to fork");
        let absent =
            parse(r#"{"run_id":"run","conversation_id":"conversation","fork_supported":"true"}"#);
        assert!(!absent.fork_supported);
        assert!(absent.fork_reason.is_empty());
    }

    #[test]
    fn child_identity_uses_the_inspected_conversation_and_checks_parent_and_child() {
        let t = Target {
            computer: "mac".into(),
            name: "codex/sample-project/review".into(),
            run_id: "r1".into(),
            conversation_id: "parent".into(),
        };
        let call = subagent_identity_invocation(&t, Some("a1"), "child").unwrap();
        assert_eq!(
            call.args,
            [
                "@mac",
                "inspect",
                "codex/sample-project/review",
                "--after",
                "0",
                "--skip-processes",
                "--archive",
                "a1",
                "--subagent",
                "child"
            ]
        );
        assert_eq!(call.timeout, Duration::from_secs(15));
        for id in ["", "../child", "child\\other", "child\n"] {
            assert!(subagent_identity_invocation(&t, None, id).is_none());
        }
        let receipt = serde_json::json!({"name":t.name,"run_id":"r1","parent_conversation_id":"parent","agent_id":"child","conversation_id":"child-conversation"});
        let exit = |v: &Value| Exit::Code {
            code: 0,
            stdout: serde_json::to_vec(v).unwrap(),
            stderr: String::new(),
        };
        assert_eq!(
            parse_subagent_identity(&t, "child", exit(&receipt))
                .unwrap()
                .conversation_id,
            "child-conversation"
        );
        for key in ["name", "run_id", "parent_conversation_id", "agent_id"] {
            let mut wrong = receipt.clone();
            wrong[key] = serde_json::json!("other");
            assert!(
                parse_subagent_identity(&t, "child", exit(&wrong)).is_none(),
                "{key}"
            );
        }
        let mut empty = receipt;
        empty["conversation_id"] = serde_json::json!("");
        assert!(parse_subagent_identity(&t, "child", exit(&empty)).is_none());
    }

    #[test]
    fn child_activity_keeps_one_bounded_page_of_the_exact_child() {
        let t = Target {
            computer: String::new(),
            name: "dsh/sample-project/review".into(),
            run_id: "r1".into(),
            conversation_id: "parent".into(),
        };
        let exit = |value: &Value| Exit::Code {
            code: 0,
            stdout: serde_json::to_vec(value).unwrap(),
            stderr: String::new(),
        };
        let page = serde_json::json!({"name":t.name,"run_id":"r1","parent_conversation_id":"parent","agent_id":"child","conversation_id":"parent/child","send_supported":true,"events":[{"seq":1,"type":"Stop"}]});
        assert_eq!(
            parse_subagent_activity(&t, "child", exit(&page)),
            Some(page.clone())
        );
        let mut wrong = page.clone();
        wrong["parent_conversation_id"] = serde_json::json!("other");
        assert!(parse_subagent_activity(&t, "child", exit(&wrong)).is_none());
        let mut missing = page.clone();
        missing.as_object_mut().unwrap().remove("events");
        assert!(parse_subagent_activity(&t, "child", exit(&missing)).is_none());
        let mut scalar = page.clone();
        scalar["events"] = serde_json::json!([1]);
        assert!(parse_subagent_activity(&t, "child", exit(&scalar)).is_none());
        let mut oversized = page;
        oversized["events"] = serde_json::json!(vec![serde_json::json!({}); 201]);
        assert!(parse_subagent_activity(&t, "child", exit(&oversized)).is_none());
    }

    #[test]
    fn inspect_call_matches_the_qt_client() {
        let t = Target {
            computer: "mac".into(),
            name: "claude/sample-project/review".into(),
            run_id: "run-1".into(),
            conversation_id: "conv-1".into(),
        };
        let inv = details_invocation(&t, Some("a1"));
        assert_eq!(
            inv.args,
            [
                "@mac",
                "inspect",
                "claude/sample-project/review",
                "--after",
                "0",
                "--skip-processes",
                "--archive",
                "a1"
            ]
        );
        assert_eq!(inv.timeout, std::time::Duration::from_secs(15));
        assert_eq!(inv.stdin, None);
        assert_eq!(inv.request_id, None);
    }

    #[test]
    fn a_local_call_has_no_alias_and_five_seconds() {
        let t = Target {
            computer: String::new(),
            name: "claude/sample-project/review".into(),
            run_id: "run-1".into(),
            conversation_id: "conv-1".into(),
        };
        let inv = details_invocation(&t, None);
        assert_eq!(
            inv.args,
            [
                "inspect",
                "claude/sample-project/review",
                "--after",
                "0",
                "--skip-processes"
            ]
        );
        assert_eq!(inv.timeout, std::time::Duration::from_secs(5));
    }

    #[test]
    fn details_are_adopted_only_for_the_listed_run() {
        let t = Target {
            computer: String::new(),
            name: "claude/sample-project/review".into(),
            run_id: "run-1".into(),
            conversation_id: "conv-1".into(),
        };
        let body = br#"{"run_id":"run-1","conversation_id":"conv-1","expected_id":"e1","resume_message_can_send":false}"#;
        let d = parse_details(
            &t,
            Exit::Code {
                code: 0,
                stdout: body.to_vec(),
                stderr: String::new(),
            },
        )
        .unwrap();
        assert_eq!(d.expected_id, "e1");
        assert!(!d.resume_message_can_send);
        assert_eq!(d.allowed_actions, None);
        let other = br#"{"run_id":"run-2","conversation_id":"conv-1"}"#;
        assert_eq!(
            parse_details(
                &t,
                Exit::Code {
                    code: 0,
                    stdout: other.to_vec(),
                    stderr: String::new()
                }
            ),
            None
        );
        assert_eq!(
            parse_details(
                &t,
                Exit::Code {
                    code: 1,
                    stdout: body.to_vec(),
                    stderr: String::new()
                }
            ),
            None
        );
        assert_eq!(
            parse_details(
                &t,
                Exit::Code {
                    code: 0,
                    stdout: b"not json".to_vec(),
                    stderr: String::new()
                }
            ),
            None
        );
    }

    #[test]
    fn allowed_actions_and_reasons_are_read() {
        let t = Target {
            computer: String::new(),
            name: "n/s".into(),
            run_id: "r".into(),
            conversation_id: "c".into(),
        };
        let body = br#"{"run_id":"r","conversation_id":"c","auth_required":true,"allowed_actions":["pause"],"action_reasons":{"resume":"busy"}}"#;
        let d = parse_details(
            &t,
            Exit::Code {
                code: 0,
                stdout: body.to_vec(),
                stderr: String::new(),
            },
        )
        .unwrap();
        assert!(d.auth_required);
        assert_eq!(d.allowed_actions, Some(vec!["pause".to_owned()]));
        assert_eq!(
            d.action_reasons.get("resume").map(String::as_str),
            Some("busy")
        );
        assert!(!d.interrupt_supported);
        assert_eq!((d.turn_started, d.phase.as_str()), (0.0, ""));
    }

    #[test]
    fn the_interrupt_fields_are_read() {
        let t = Target {
            computer: String::new(),
            name: "claude/sample-project/review".into(),
            run_id: "r".into(),
            conversation_id: "c".into(),
        };
        let body = br#"{"run_id":"r","conversation_id":"c","phase":"tool","turn_started":1793000000.5,"interrupt_supported":true}"#;
        let d = parse_details(
            &t,
            Exit::Code {
                code: 0,
                stdout: body.to_vec(),
                stderr: String::new(),
            },
        )
        .unwrap();
        assert!(d.interrupt_supported);
        assert_eq!(d.turn_started, 1793000000.5);
        assert_eq!(d.phase, "tool");
    }

    #[test]
    fn cwd_and_terminal_support_come_from_the_inspection() {
        let t = Target {
            computer: String::new(),
            name: "n".into(),
            run_id: "r".into(),
            conversation_id: "c".into(),
        };
        let exit = |json: &str| Exit::Code {
            code: 0,
            stdout: json.as_bytes().to_vec(),
            stderr: String::new(),
        };
        let none = parse_details(&t, exit(r#"{"run_id":"r","conversation_id":"c"}"#)).unwrap();
        assert_eq!(none.cwd, None);
        assert_eq!(
            (none.terminal_supported, none.terminal_attach_supported),
            (None, None)
        );
        let some = parse_details(
            &t,
            exit(r#"{"run_id":"r","conversation_id":"c","cwd":"/home/example/x","terminal_supported":false,"terminal_attach_supported":true}"#),
        )
        .unwrap();
        assert_eq!(some.cwd.as_deref(), Some("/home/example/x"));
        let empty =
            parse_details(&t, exit(r#"{"run_id":"r","conversation_id":"c","cwd":""}"#)).unwrap();
        assert_eq!(empty.cwd.as_deref(), Some(""));
        assert_eq!(some.terminal_supported, Some(false));
        assert_eq!(some.terminal_attach_supported, Some(true));
    }

    // G1: Qt reads `m_details.value("cwd_canonical").toString(row's)`: the
    // row's form stands in only when the inspection has no string.
    #[test]
    fn the_canonical_folder_comes_from_the_inspection_when_it_is_a_string() {
        let t = Target {
            computer: String::new(),
            name: "n".into(),
            run_id: "r".into(),
            conversation_id: "c".into(),
        };
        let exit = |json: &str| Exit::Code {
            code: 0,
            stdout: json.as_bytes().to_vec(),
            stderr: String::new(),
        };
        let some = parse_details(
            &t,
            exit(r#"{"run_id":"r","conversation_id":"c","cwd_canonical":"/srv/example/x"}"#),
        )
        .unwrap();
        assert_eq!(some.cwd_canonical.as_deref(), Some("/srv/example/x"));
        let none = parse_details(
            &t,
            exit(r#"{"run_id":"r","conversation_id":"c","cwd_canonical":2}"#),
        )
        .unwrap();
        assert_eq!(none.cwd_canonical, None);
    }
}
