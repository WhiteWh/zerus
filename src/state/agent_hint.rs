//! Optional static coordination context for tracked main conversations.
//!
//! A binding mark suppresses another attempt; it is not a receipt proving
//! stdout delivery or native acceptance. State persistence and a provider's
//! input pipe cannot provide crash-atomic exactly-once delivery. An output
//! failure never rolls back the mark or automatically retries the hint.
use super::*;
use std::io::Write;

/// Revision-one bytes are intentionally independent of hgs version and names.
pub(super) const HINT: &str = "This agent runs in a Zerus session: the hgs CLI reads, messages, \
launches and stops other agent sessions on this and other computers. Run hgs guide before using it.";
const REVISION: u64 = 1;
pub(super) const FORK_REVISION_ENV: &str = "HGS_FORK_AGENT_HINT_REVISION";

fn supported(record: &Value) -> bool {
    ["claude", "codex", "kimi"].contains(&string(record, "agent"))
}

fn marked(record: &Value, conversation: &str) -> bool {
    !conversation.is_empty()
        && record["agent_hint"]["conversation_id"] == conversation
        && record["agent_hint"]["revision"] == REVISION
}

/// Called with the exact selected live/saved/archive source under the fork
/// writer lock. No later lookup by mutable source name establishes inheritance.
pub(super) fn fork_revision(source: &Value) -> String {
    if supported(source) && marked(source, string(source, "conversation_id")) {
        REVISION.to_string()
    } else {
        String::new()
    }
}

/// Consume the supervisor's captured fork evidence before spawning the native
/// process. The child conversation is still unknown, so do not mark it yet.
pub(super) fn inherit_fork(record: &mut Value, revision: Option<&str>) {
    let parent = string(record, "fork_parent_id");
    if supported(record)
        && revision == Some("1")
        && !parent.is_empty()
        && !string(record, "fork_source_name").is_empty()
    {
        record["agent_hint_inherited"] = json!({"conversation_id":parent,"revision":REVISION});
    }
}

/// A host or native session receives the fixed hint unless its account opted out.
pub(super) fn for_account(account: &str) -> Option<&'static str> {
    crate::config::configured_agent_hint(account).then_some(HINT)
}

/// Only the lifecycle's already-verified same-conversation resume calls this.
/// A new run ID is expected, while provider and conversation must still match.
pub(super) fn carry_resume(previous: &Value, record: &mut Value) {
    let conversation = string(record, "conversation_id");
    if supported(record)
        && record["agent"] == previous["agent"]
        && conversation == string(previous, "conversation_id")
        && marked(previous, conversation)
    {
        record["agent_hint"] = previous["agent_hint"].clone();
    }
}

/// The caller has admitted and bound the main native start or prompt event
/// through the normal run/pane/provider guards. Optional policy failure cannot
/// break tracking.
pub(super) fn start(record: &mut Value, event: &Value) -> Option<&'static str> {
    if !admitted(record, event) {
        return None;
    }
    let account = match string(record, "account_id") {
        "" => format!("native-{}", string(record, "agent")),
        account => account.to_owned(),
    };
    decide(record, event, crate::config::resolved_agent_hint(&account))
}

fn admitted(record: &Value, event: &Value) -> bool {
    let expected = if string(record, "agent") == "kimi" {
        "UserPromptSubmit"
    } else {
        "SessionStart"
    };
    supported(record)
        && string(event, "hook_event_name") == expected
        && string(event, "agent_id").is_empty()
        && !string(event, "session_id").is_empty()
        && event["session_id"] == record["conversation_id"]
}

fn decide(record: &mut Value, event: &Value, enabled: bool) -> Option<&'static str> {
    if !admitted(record, event) {
        return None;
    }
    let conversation = string(record, "conversation_id").to_owned();
    let source = string(event, "source");
    let replaced = string(record, "agent") != "kimi" && matches!(source, "compact" | "clear");
    let inherited = record
        .as_object_mut()
        .unwrap()
        .remove("agent_hint_inherited");
    if replaced || record["agent_hint"]["conversation_id"] != conversation {
        record.as_object_mut().unwrap().remove("agent_hint");
    }
    if !replaced
        && inherited.is_some_and(|mark| {
            !string(record, "fork_parent_id").is_empty()
                && string(record, "fork_parent_id") != conversation
                && mark["conversation_id"] == record["fork_parent_id"]
                && mark["revision"] == REVISION
        })
    {
        // This context already existed in the exact selected source. Its
        // suppression mark may transfer even while new injection is disabled.
        record["agent_hint"] = json!({"conversation_id":conversation,"revision":REVISION});
    }
    if !enabled || marked(record, &conversation) {
        return None;
    }
    record["agent_hint"] = json!({"conversation_id":conversation,"revision":REVISION});
    Some(HINT)
}

/// One stable UTF-8 JSON document, serialized before the first stdout write.
/// Field order is explicit and does not depend on serde_json map features.
pub(super) fn output(writer: &mut impl Write, context: &str) -> Result<()> {
    #[derive(serde::Serialize)]
    struct Fields<'a> {
        #[serde(rename = "additionalContext")]
        context: &'a str,
        #[serde(rename = "hookEventName")]
        event: &'static str,
    }
    #[derive(serde::Serialize)]
    struct Output<'a> {
        #[serde(rename = "hookSpecificOutput")]
        fields: Fields<'a>,
    }
    let output = Output {
        fields: Fields {
            context,
            event: "SessionStart",
        },
    };
    let mut bytes =
        serde_json::to_vec(&output).map_err(|_| "Could not encode SessionStart context")?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .and_then(|()| writer.flush())
        .map_err(|_| "Could not write SessionStart context; delivery is unknown".into())
}

pub(super) fn output_for(agent: &str, writer: &mut impl Write, context: &str) -> Result<()> {
    if agent != "kimi" {
        return output(writer, context);
    }
    #[derive(serde::Serialize)]
    struct Output<'a> {
        message: &'a str,
    }
    let mut bytes = serde_json::to_vec(&Output { message: context })
        .map_err(|_| "Could not encode Kimi UserPromptSubmit context")?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .and_then(|()| writer.flush())
        .map_err(|_| "Could not write Kimi UserPromptSubmit context; delivery is unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(agent: &str, conversation: &str) -> Value {
        json!({"agent":agent,"run_id":"run","conversation_id":conversation})
    }

    fn event(conversation: &str, source: &str) -> Value {
        json!({"hook_event_name":"SessionStart","session_id":conversation,"source":source})
    }

    #[test]
    fn agent_hint_lifecycle_context_replacement_and_pre_feature_resume() {
        for agent in ["claude", "codex"] {
            let mut run = record(agent, "one");
            assert_eq!(decide(&mut run, &event("one", "startup"), true), Some(HINT));
            for source in ["resume", "startup", "resume", ""] {
                assert_eq!(decide(&mut run, &event("one", source), true), None);
            }
            let previous = run.clone();
            let mut resumed = record(agent, "one");
            resumed["run_id"] = json!("new-run");
            carry_resume(&previous, &mut resumed);
            assert_eq!(decide(&mut resumed, &event("one", "resume"), true), None);
            for source in ["compact", "compact"] {
                assert_eq!(
                    decide(&mut resumed, &event("one", source), true),
                    Some(HINT)
                );
                assert_eq!(decide(&mut resumed, &event("one", "resume"), true), None);
            }
            resumed["conversation_id"] = json!("two");
            assert_eq!(
                decide(&mut resumed, &event("two", "clear"), true),
                Some(HINT)
            );
            assert_eq!(decide(&mut resumed, &event("two", "resume"), true), None);
            let mut earlier = record(agent, "pre-feature");
            assert_eq!(
                decide(&mut earlier, &event("pre-feature", "resume"), true),
                Some(HINT)
            );
            assert_eq!(
                decide(&mut earlier, &event("pre-feature", "resume"), true),
                None
            );
            earlier["agent_hint"]["revision"] = json!(0);
            assert_eq!(
                decide(&mut earlier, &event("pre-feature", "resume"), true),
                Some(HINT)
            );
        }
    }

    #[test]
    fn agent_hint_off_does_not_invent_delivery_and_replacement_invalidates_old_context() {
        let mut run = record("claude", "one");
        assert_eq!(decide(&mut run, &event("one", "startup"), false), None);
        assert!(run.get("agent_hint").is_none());
        assert_eq!(decide(&mut run, &event("one", "resume"), true), Some(HINT));
        assert_eq!(decide(&mut run, &event("one", "compact"), false), None);
        assert!(run.get("agent_hint").is_none());
        assert_eq!(decide(&mut run, &event("one", "resume"), true), Some(HINT));
    }

    #[test]
    fn agent_hint_lost_mark_selects_another_output_without_claiming_native_emission() {
        for agent in ["claude", "codex"] {
            let mut run = record(agent, "one");
            let mut writes = Vec::new();
            output(
                &mut writes,
                decide(&mut run, &event("one", "startup"), true).unwrap(),
            )
            .unwrap();
            assert_eq!(decide(&mut run, &event("one", "resume"), true), None);
            run.as_object_mut().unwrap().remove("agent_hint");
            output(
                &mut writes,
                decide(&mut run, &event("one", "resume"), true).unwrap(),
            )
            .unwrap();
            let lines = std::str::from_utf8(&writes)
                .unwrap()
                .lines()
                .collect::<Vec<_>>();
            assert_eq!(lines.len(), 2);
            assert_eq!(lines[0], lines[1]);
            // These are writes into this pure control's buffer. No native
            // process, retained transcript or model-context count is observed.
            assert!(marked(&run, "one"));
        }
    }

    #[test]
    fn agent_hint_fork_uses_captured_source_including_archives_without_later_name_lookup() {
        for agent in ["claude", "codex"] {
            let mut source = record(agent, "parent");
            decide(&mut source, &event("parent", "startup"), true);
            source["archive_id"] = json!("saved-archive");
            let revision = fork_revision(&source);
            assert_eq!(revision, "1");
            source["conversation_id"] = json!("later");
            assert_eq!(fork_revision(&source), "");
            let mut child = record(agent, "child");
            child["fork_parent_id"] = json!("parent");
            child["fork_source_name"] = json!("now-renamed-or-removed");
            inherit_fork(&mut child, Some(&revision));
            assert!(child.get("agent_hint").is_none());
            assert_eq!(decide(&mut child, &event("child", "fork"), false), None);
            assert!(marked(&child, "child"));
            assert!(child.get("agent_hint_inherited").is_none());
            assert_eq!(decide(&mut child, &event("child", "resume"), true), None);
            assert_eq!(
                decide(&mut child, &event("child", "compact"), true),
                Some(HINT)
            );
            child["conversation_id"] = json!("fresh-child");
            assert_eq!(
                decide(&mut child, &event("fresh-child", "clear"), true),
                Some(HINT)
            );
            let mut unmarked = record(agent, "unmarked-child");
            unmarked["fork_parent_id"] = json!("parent");
            unmarked["fork_source_name"] = json!("source");
            inherit_fork(&mut unmarked, Some(""));
            assert_eq!(
                decide(&mut unmarked, &event("unmarked-child", "fork"), true),
                Some(HINT)
            );
        }
    }

    #[test]
    fn agent_hint_admission_rejects_foreign_children_events_and_wrong_resume_scope() {
        let mut original = record("claude", "one");
        decide(&mut original, &event("one", "startup"), true);
        original["agent_hint_inherited"] = json!({"conversation_id":"parent","revision":1});
        for (agent, conversation, kind, child) in [
            ("opencode", "one", "SessionStart", ""),
            ("dsh", "one", "SessionStart", ""),
            ("claude", "other", "SessionStart", ""),
            ("claude", "", "SessionStart", ""),
            ("claude", "one", "Stop", ""),
            ("claude", "one", "SessionStart", "native-child"),
        ] {
            let mut run = original.clone();
            run["agent"] = json!(agent);
            let mut start = event(conversation, "startup");
            start["hook_event_name"] = json!(kind);
            start["agent_id"] = json!(child);
            let before = run.clone();
            assert_eq!(decide(&mut run, &start, true), None);
            assert_eq!(run, before);
        }
        for (agent, conversation) in [("codex", "one"), ("claude", "other"), ("claude", "")] {
            let mut resumed = record(agent, conversation);
            carry_resume(&original, &mut resumed);
            assert!(resumed.get("agent_hint").is_none());
        }
    }

    #[test]
    fn kimi_prompt_hint_is_once_per_conversation_without_compact_reemission() {
        let prompt = |conversation: &str, source: &str| {
            let mut event = event(conversation, source);
            event["hook_event_name"] = json!("UserPromptSubmit");
            event
        };
        let mut run = record("kimi", "one");
        assert_eq!(
            decide(&mut run, &prompt("one", "startup"), true),
            Some(HINT)
        );
        for source in ["resume", "startup", "compact", "resume"] {
            assert_eq!(decide(&mut run, &prompt("one", source), true), None);
        }
        let previous = run.clone();
        let mut resumed = record("kimi", "one");
        resumed["run_id"] = json!("new-run");
        carry_resume(&previous, &mut resumed);
        for kind in ["SessionStart", "PreCompact", "PostCompact"] {
            let mut lifecycle = event("one", "resume");
            lifecycle["hook_event_name"] = json!(kind);
            assert_eq!(decide(&mut resumed, &lifecycle, true), None);
            assert!(marked(&resumed, "one"));
        }
        assert_eq!(decide(&mut resumed, &prompt("one", "resume"), true), None);
        resumed["conversation_id"] = json!("two");
        assert_eq!(
            decide(&mut resumed, &prompt("two", "clear"), true),
            Some(HINT)
        );
        assert_eq!(decide(&mut resumed, &prompt("two", "resume"), true), None);
        let mut wrong = record("kimi", "one");
        assert_eq!(decide(&mut wrong, &event("one", "startup"), true), None);
        let mut bytes = Vec::new();
        output_for("kimi", &mut bytes, HINT).unwrap();
        let mut expected = serde_json::to_vec(&json!({"message":HINT})).unwrap();
        expected.push(b'\n');
        assert_eq!(bytes, expected);
    }

    #[test]
    fn agent_hint_changes_only_suppression_metadata_and_preserves_instruction_argv_and_envelope() {
        let mut run = record("claude", "one");
        run["name"] = json!("claude/example/renamed");
        run["base"] = json!([
            "project-launcher",
            "--append-system-prompt",
            "user append",
            "--system-prompt",
            "user system",
            "--settings",
            "user.json"
        ]);
        run["prompt"] = json!("<zerus-message>peer-authored text</zerus-message>");
        run["hook_health"] = json!({"events":{"SessionStart":1}});
        let before = run.clone();
        decide(&mut run, &event("one", "startup"), true);
        assert!(marked(&run, "one"));
        run.as_object_mut().unwrap().remove("agent_hint");
        assert_eq!(run, before);
        let mut bytes = Vec::new();
        output(&mut bytes, HINT).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({"hookSpecificOutput":{
            "hookEventName":"SessionStart","additionalContext":HINT}})
        );
        assert!(HINT.is_ascii() && HINT.len() <= 220 && !HINT.contains('\n'));
        assert_eq!(HINT, "This agent runs in a Zerus session: the hgs CLI reads, messages, launches and stops other agent sessions on this and other computers. Run hgs guide before using it.");
    }

    #[test]
    fn session_start_combines_hint_and_tasks_in_one_stable_document() {
        let tasks = crate::tasks::session_context("claude").unwrap();
        let text = format!(
            "{HINT}\n\n{}",
            tasks["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
        );
        let mut bytes = Vec::new();
        output(&mut bytes, &text).unwrap();
        assert_eq!(
            bytes,
            include_bytes!("../../tests/fixtures/session-start-context.json")
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["hookSpecificOutput"]
                ["additionalContext"],
            text
        );
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert_eq!(bytes.iter().filter(|&&b| b == b'\n').count(), 1);
        assert!(!bytes.starts_with(&[0xef, 0xbb, 0xbf]));
    }

    #[test]
    fn agent_hint_output_failure_is_unknown_without_mark_rollback_or_retry() {
        struct Fault {
            writes: usize,
            flushes: usize,
            fail_write: bool,
        }
        impl Write for Fault {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.writes += 1;
                if self.fail_write {
                    Err(io::Error::from(io::ErrorKind::BrokenPipe))
                } else {
                    Ok(bytes.len())
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                self.flushes += 1;
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }
        }
        for fail_write in [true, false] {
            let mut run = record("codex", "one");
            let hint = decide(&mut run, &event("one", "startup"), true).unwrap();
            let mut writer = Fault {
                writes: 0,
                flushes: 0,
                fail_write,
            };
            assert!(output(&mut writer, hint)
                .unwrap_err()
                .contains("delivery is unknown"));
            assert_eq!(
                (writer.writes, writer.flushes),
                (1, usize::from(!fail_write))
            );
            assert!(marked(&run, "one"));
            assert_eq!(decide(&mut run, &event("one", "resume"), true), None);
        }
    }
}
