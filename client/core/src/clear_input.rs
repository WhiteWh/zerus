//! Terminal's Clear input (zerus-wkj9l): one bounded read of the native
//! composer and the explicit clear of exactly the text the person confirmed,
//! both through `hgs [@alias] terminal NAME --json`. The CLI verifies the
//! session, binding and text before any key and never replays a request; the
//! desktop only builds the request and reads its answer. Never automatic and
//! never part of a delivery retry.
use crate::actions::{prepend_alias, Exit, Invocation, Target};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

/// The longest composer text the CLI returns (its receipt bound).
pub const TEXT_CHARS: usize = 2_000;
/// How long the clear's terminal handoff stays valid (the CLI admits up to 6 s).
pub const HANDOFF_SECONDS: f64 = 5.0;

/// What the composer read says.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComposerState {
    /// The terminal binding the clear must name.
    pub binding: String,
    /// `empty`, `text` or `unknown`.
    pub state: String,
    pub text: String,
    pub clearable: bool,
    pub reason: String,
}

/// What the clear did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClearOutcome {
    /// `cleared`, `failed` (nothing typed) or `uncertain` (keys were sent).
    pub status: String,
    pub error: String,
    /// What the composer still held when it was uncertain.
    pub composer: Option<String>,
}

fn request(target: &Target, request_id: &str, action: &str) -> Option<Value> {
    if target.name.is_empty() || target.run_id.is_empty() || uuid::Uuid::parse_str(request_id).is_err() {
        return None;
    }
    Some(json!({"request_id": request_id, "action": action, "expected_run_id": target.run_id,
        "expected_conversation_id": target.conversation_id}))
}

fn invocation(target: &Target, input: Value, request_id: &str) -> Invocation {
    let mut args = vec!["terminal".to_owned(), target.name.clone(), "--json".to_owned()];
    let peer = prepend_alias(&mut args, &target.computer);
    Invocation {
        args,
        stdin: Some(input.to_string().into_bytes()),
        timeout: Duration::from_secs(if peer { 20 } else { 10 }),
        request_id: Some(request_id.to_owned()),
    }
}

/// The composer read of `target`'s live terminal.
pub fn composer_invocation(target: &Target, request_id: &str) -> Option<Invocation> {
    Some(invocation(target, request(target, request_id, "composer")?, request_id))
}

/// The clear of exactly `text`, bound to the read's `binding`, valid until
/// `now + HANDOFF_SECONDS`.
pub fn clear_invocation(target: &Target, request_id: &str, binding: &str, text: &str, now: f64) -> Option<Invocation> {
    let mut input = request(target, request_id, "clear-input")?;
    if binding.len() != 64
        || !binding.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || text.trim().is_empty()
        || text.chars().count() > TEXT_CHARS
    {
        return None;
    }
    input["terminal_binding_id"] = json!(binding);
    input["text"] = json!(text);
    input["expires_at"] = json!(now + HANDOFF_SECONDS);
    Some(invocation(target, input, request_id))
}

fn answer(exit: &Exit, target: &Target, request_id: &str, status: &[&str]) -> Option<Value> {
    let Exit::Code { code: 0, stdout, .. } = exit else {
        return None;
    };
    let value: Value = serde_json::from_slice(stdout).ok()?;
    (value["request_id"] == request_id
        && value["name"] == target.name.as_str()
        && value["run_id"] == target.run_id.as_str()
        && status.contains(&value["status"].as_str()?))
    .then_some(value)
}

/// The read's answer, only for exactly this request and session.
pub fn parse_composer(exit: &Exit, target: &Target, request_id: &str) -> Option<ComposerState> {
    let value = answer(exit, target, request_id, &["composer"])?;
    let composer = &value["composer"];
    let state = composer["state"].as_str()?;
    if !matches!(state, "empty" | "text" | "unknown") {
        return None;
    }
    let text = composer["text"].as_str()?;
    Some(ComposerState {
        binding: value["terminal_binding_id"].as_str()?.to_owned(),
        state: state.to_owned(),
        text: text.chars().take(TEXT_CHARS).collect(),
        clearable: composer["clearable"].as_bool()? && state == "text" && !text.trim().is_empty(),
        reason: composer["reason"].as_str().unwrap_or_default().to_owned(),
    })
}

/// The clear's answer. Anything but an exact answer for this request is
/// uncertain: keys may have been sent.
pub fn parse_clear(exit: &Exit, target: &Target, request_id: &str) -> ClearOutcome {
    match answer(exit, target, request_id, &["cleared", "failed", "uncertain"]) {
        Some(value) => ClearOutcome {
            status: value["status"].as_str().unwrap_or("uncertain").to_owned(),
            error: value["error"].as_str().unwrap_or_default().to_owned(),
            composer: value["composer"].as_str().map(|text| text.chars().take(TEXT_CHARS).collect()),
        },
        None => ClearOutcome {
            status: "uncertain".into(),
            error: match exit {
                Exit::Code { stderr, .. } | Exit::Abnormal { stderr } if !stderr.trim().is_empty() => stderr.trim().to_owned(),
                _ => "The clear did not report its outcome. Check Terminal.".into(),
            },
            composer: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "11111111-1111-4111-8111-111111111111";
    fn target(computer: &str) -> Target {
        Target { computer: computer.into(), name: "claude/p/s".into(), run_id: "r".into(), conversation_id: "c".into() }
    }
    fn exit(value: Value) -> Exit {
        Exit::Code { code: 0, stdout: value.to_string().into_bytes(), stderr: String::new() }
    }
    const BINDING: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn requests_name_the_session_binding_and_exact_text() {
        let read = composer_invocation(&target(""), ID).unwrap();
        assert_eq!(read.args, ["terminal", "claude/p/s", "--json"]);
        let input: Value = serde_json::from_slice(read.stdin.as_ref().unwrap()).unwrap();
        assert_eq!(input, json!({"request_id": ID, "action": "composer", "expected_run_id": "r", "expected_conversation_id": "c"}));
        let clear = clear_invocation(&target("peer"), ID, BINDING, "[Image #1] residue", 100.0).unwrap();
        assert_eq!(&clear.args[..2], ["@peer", "terminal"]);
        let input: Value = serde_json::from_slice(clear.stdin.as_ref().unwrap()).unwrap();
        assert_eq!((input["action"].as_str(), input["text"].as_str(), input["expires_at"].as_f64()), (Some("clear-input"), Some("[Image #1] residue"), Some(105.0)));
        assert!(clear_invocation(&target(""), ID, "short", "x", 1.0).is_none());
        assert!(clear_invocation(&target(""), ID, BINDING, "  ", 1.0).is_none());
        assert!(clear_invocation(&target(""), ID, BINDING, &"x".repeat(TEXT_CHARS + 1), 1.0).is_none());
        assert!(composer_invocation(&Target { run_id: String::new(), ..target("") }, ID).is_none());
    }

    #[test]
    fn only_this_requests_answer_counts() {
        let ok = json!({"request_id": ID, "name": "claude/p/s", "run_id": "r", "status": "composer", "terminal_binding_id": BINDING,
            "composer": {"state": "text", "text": "residue", "clearable": true, "reason": ""}});
        let state = parse_composer(&exit(ok.clone()), &target(""), ID).unwrap();
        assert_eq!((state.state.as_str(), state.text.as_str(), state.clearable, state.binding.as_str()), ("text", "residue", true, BINDING));
        let mut other = ok.clone();
        other["run_id"] = json!("another run");
        assert!(parse_composer(&exit(other), &target(""), ID).is_none());
        let mut empty = ok;
        empty["composer"] = json!({"state": "empty", "text": "", "clearable": true, "reason": ""});
        assert!(!parse_composer(&exit(empty), &target(""), ID).unwrap().clearable, "an empty composer is never clearable");
        let cleared = parse_clear(&exit(json!({"request_id": ID, "name": "claude/p/s", "run_id": "r", "status": "cleared"})), &target(""), ID);
        assert_eq!(cleared.status, "cleared");
        let lost = parse_clear(&Exit::TimedOut, &target(""), ID);
        assert_eq!(lost.status, "uncertain", "keys may have been sent");
        let refused = parse_clear(&exit(json!({"request_id": ID, "name": "claude/p/s", "run_id": "r", "status": "failed",
            "error": "The input changed; review it again before clearing"})), &target(""), ID);
        assert_eq!((refused.status.as_str(), refused.error.as_str()), ("failed", "The input changed; review it again before clearing"));
    }
}
