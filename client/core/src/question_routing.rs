//! Where a supervised child's questions go (`hgs [@host] question-routing
//! NAME --json`, zerus-sa9qn.32): the person's per-child switch of the
//! owner-approved question route. This only asks the child's host and
//! reports what it confirmed. The CLI refuses a change from an agent's
//! process tree, a guard against accidental agent use rather than a boundary;
//! the person-only control is the global switch (zerus-sa9qn.49).
use crate::actions::{help_invocation, prepend_alias, Exit, Invocation, Target};
use crate::source;
use serde_json::{json, Value};
use std::time::Duration;

/// The CLI's help marker for the switch.
pub const MARKER: &str = "session-question-routing-v1";
const UPDATE: &str =
    source!("Update Zerus on this computer to choose where a child's questions go.");
const UNCONFIRMED: &str =
    source!("The computer did not confirm where this child's questions go. Refresh the session.");
const IDENTITY: &str = source!("The session has no confirmed conversation yet.");
const HELP_CAP: usize = 256 * 1024;
const REPLY_CAP: usize = 64 * 1024;

/// Whether `stdout` lists the marker as a whole line.
pub fn has_marker(stdout: &[u8]) -> bool {
    stdout
        .split(|&b| b == b'\n')
        .any(|line| line.trim_ascii() == MARKER.as_bytes())
}

/// `hgs [@host] question-routing NAME --json`; 15 s here and 35 s on a peer.
pub fn invocation(
    target: &Target,
    send_to_parent: bool,
    expected_revision: i64,
) -> Result<Invocation, String> {
    let identity = |value: &str, maximum: usize| {
        !value.is_empty()
            && value.len() <= maximum
            && !value.chars().any(|c| c < ' ' || c == '\u{7f}')
    };
    if target.name.is_empty()
        || !identity(&target.run_id, 128)
        || !identity(&target.conversation_id, 256)
    {
        return Err(IDENTITY.to_owned());
    }
    if expected_revision < 0 {
        return Err(UNCONFIRMED.to_owned());
    }
    let input = json!({
        "expected_run_id": target.run_id,
        "expected_conversation_id": target.conversation_id,
        "expected_revision": expected_revision,
        "send_to_parent": send_to_parent,
    });
    let mut args = vec![
        "question-routing".to_owned(),
        target.name.clone(),
        "--json".to_owned(),
    ];
    let peer = prepend_alias(&mut args, &target.computer);
    Ok(Invocation {
        args,
        stdin: Some(serde_json::to_vec(&input).map_err(|error| error.to_string())?),
        timeout: Duration::from_secs(if peer { 35 } else { 15 }),
        request_id: None,
    })
}

/// The words for a refusal code the host gave.
fn refusal(error: &str) -> &'static str {
    match error {
        "conflict" => source!(
            "Where this child's questions go changed on another device. Refresh the session and try again."
        ),
        "stale_identity" | "not_found" => source!("The session changed. Refresh it and try again."),
        "unsupported" => source!("This session has no durable identity, so its questions cannot be routed."),
        "agent_refused" => source!("Only you can change where a child's questions go."),
        _ => UNCONFIRMED,
    }
}

/// The confirmed `question_routing` object, or the reason the host gave.
pub fn classify(
    target: &Target,
    send_to_parent: bool,
    expected_revision: i64,
    exit: Exit,
) -> Result<Value, String> {
    let (code, stdout) = match &exit {
        Exit::Code { code, stdout, .. } => (*code, stdout.as_slice()),
        _ => return Err(UNCONFIRMED.to_owned()),
    };
    let reply = serde_json::from_slice::<Value>(stdout)
        .ok()
        .filter(Value::is_object);
    if code != 0 {
        let error = reply
            .as_ref()
            .filter(|reply| reply["ok"] == false)
            .and_then(|reply| reply["error"].as_str())
            .unwrap_or("");
        return Err(refusal(error).to_owned());
    }
    let Some(mut reply) = reply else {
        return Err(UNCONFIRMED.to_owned());
    };
    let exact = reply.as_object().is_some_and(|object| {
        object.len() == 5
            && [
                "ok",
                "applied",
                "run_id",
                "conversation_id",
                "question_routing",
            ]
            .iter()
            .all(|key| object.contains_key(*key))
    });
    let routing = &reply["question_routing"];
    let revision = routing["revision"].as_i64();
    let confirmed = exact
        && reply["ok"] == true
        && reply["run_id"].as_str() == Some(target.run_id.as_str())
        && reply["conversation_id"].as_str() == Some(target.conversation_id.as_str())
        && routing["send_to_parent"].as_bool() == Some(send_to_parent)
        && match (reply["applied"].as_bool(), revision) {
            (Some(true), Some(revision)) => {
                expected_revision < i64::MAX && revision == expected_revision + 1
            }
            (Some(false), Some(revision)) => revision == expected_revision,
            _ => false,
        };
    if !confirmed {
        return Err(UNCONFIRMED.to_owned());
    }
    Ok(reply["question_routing"].take())
}

/// The whole change: the local help probe, the call and its classification. Blocks.
pub fn change(
    hgs: &std::path::Path,
    target: &Target,
    send_to_parent: bool,
    expected_revision: i64,
) -> Result<Value, String> {
    let call = invocation(target, send_to_parent, expected_revision)?;
    let probe = crate::run::run_limited(hgs, &help_invocation(), &[], HELP_CAP);
    if !probe
        .as_ref()
        .is_ok_and(|exit| matches!(exit, Exit::Code { code: 0, stdout, .. } if has_marker(stdout)))
    {
        return Err(UPDATE.to_owned());
    }
    let exit = crate::run::run_limited(hgs, &call, &[], REPLY_CAP).unwrap_or_else(|too_large| {
        Exit::Abnormal {
            stderr: too_large.stderr,
        }
    });
    classify(target, send_to_parent, expected_revision, exit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(computer: &str) -> Target {
        Target {
            computer: computer.into(),
            name: "claude/project/child".into(),
            run_id: "run-1".into(),
            conversation_id: "conversation-1".into(),
        }
    }
    fn reply(value: Value, code: i32) -> Exit {
        Exit::Code {
            code,
            stdout: serde_json::to_vec(&value).unwrap(),
            stderr: String::new(),
        }
    }
    fn confirmed(send: bool, revision: i64, applied: bool) -> Value {
        json!({"ok": true, "applied": applied, "run_id": "run-1", "conversation_id": "conversation-1",
            "question_routing": {"send_to_parent": send, "revision": revision}})
    }

    #[test]
    fn the_call_carries_exactly_the_identity_revision_and_choice() {
        let call = invocation(&target(""), false, 3).unwrap();
        assert_eq!(
            call.args,
            ["question-routing", "claude/project/child", "--json"]
        );
        let input: Value = serde_json::from_slice(call.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(
            input,
            json!({"expected_run_id": "run-1", "expected_conversation_id": "conversation-1",
                "expected_revision": 3, "send_to_parent": false})
        );
        assert_eq!(call.timeout, Duration::from_secs(15));
        let peer = invocation(&target("box"), true, 0).unwrap();
        assert_eq!(peer.args[0], "@box");
        assert_eq!(peer.timeout, Duration::from_secs(35));
        let mut no_run = target("");
        no_run.run_id.clear();
        assert_eq!(invocation(&no_run, true, 0).unwrap_err(), IDENTITY);
        assert!(invocation(&target(""), true, -1).is_err());
    }

    #[test]
    fn only_the_exact_confirmation_of_this_change_is_accepted() {
        let t = target("");
        assert_eq!(
            classify(&t, false, 0, reply(confirmed(false, 1, true), 0)).unwrap(),
            json!({"send_to_parent": false, "revision": 1})
        );
        assert!(classify(&t, true, 1, reply(confirmed(true, 1, false), 0)).is_ok());
        for (what, value) in [
            ("another value", confirmed(true, 1, true)),
            ("another revision", confirmed(false, 2, true)),
            ("unchanged but bumped", confirmed(false, 1, false)),
            ("another run", {
                let mut v = confirmed(false, 1, true);
                v["run_id"] = json!("run-2");
                v
            }),
            ("an extra key", {
                let mut v = confirmed(false, 1, true);
                v["extra"] = json!(1);
                v
            }),
        ] {
            assert_eq!(
                classify(&t, false, 0, reply(value, 0)).unwrap_err(),
                UNCONFIRMED,
                "{what}"
            );
        }
        let refused = |error: &str| {
            classify(&t, false, 0, reply(json!({"ok": false, "error": error}), 1)).unwrap_err()
        };
        assert!(refused("conflict").contains("another device"));
        assert!(refused("agent_refused").starts_with("Only you"));
        assert_eq!(refused("whatever"), UNCONFIRMED);
        assert_eq!(
            classify(&t, false, 0, Exit::TimedOut).unwrap_err(),
            UNCONFIRMED
        );
    }

    #[test]
    fn the_marker_must_be_a_whole_line() {
        assert!(has_marker(b"usage\n       session-question-routing-v1\r\n"));
        assert!(!has_marker(b"session-question-routing-v10\n"));
        assert!(!has_marker(b"x session-question-routing-v1\n"));
    }
}
