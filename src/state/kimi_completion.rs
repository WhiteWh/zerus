//! Kimi 2.6 no longer runs Stop at a successful main turn's end. Its bounded
//! main wire is an alternative typed completion source, never reply-text proof.
use super::*;
use std::fs;
use std::io::{Read, Seek, SeekFrom};

const MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, PartialEq)]
struct End {
    turn: u64,
    prompt_at: f64,
    at: f64,
}

// A clipped prefix grants no authority: the complete current main prompt and
// its terminal event must both be present after it, with no malformed/gap row.
fn terminal(bytes: &[u8], clipped: bool) -> Option<End> {
    let mut prompt: Option<(u64, String, f64)> = None;
    let mut end = None;
    let mut outcome = None;
    let mut conflicted = false;
    for (index, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        if clipped && index == 0 {
            continue;
        }
        if line.len() > 1024 * 1024 || !line.ends_with(b"\n") {
            return None;
        }
        let event: Value = serde_json::from_slice(line).ok()?;
        let kind = event["type"].as_str()?;
        if ["gap", "wire.gap"].contains(&kind) {
            return None;
        }
        if !["turn.prompt", "turn.ended", "prompt.completed"].contains(&kind) {
            continue;
        }
        match event["agentId"].as_str()? {
            "main" => {}
            "" => return None,
            _ => continue,
        }
        let at = event["time"]
            .as_f64()
            .filter(|at| at.is_finite() && *at > 0.0)?
            / 1000.0;
        if kind == "turn.prompt" {
            let turn = event["turnId"]
                .as_u64()
                .filter(|id| *id <= i64::MAX as u64)?;
            let id = event["promptId"].as_str().filter(|id| {
                !id.is_empty()
                    && id.len() <= 160
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
            })?;
            if prompt
                .as_ref()
                .is_some_and(|(old, _, time)| turn <= *old || at < *time)
            {
                return None;
            }
            prompt = Some((turn, id.to_owned(), at));
            end = None;
            outcome = None;
            conflicted = false;
        } else {
            let (turn, id, prompt_at) = prompt.as_ref()?;
            if at < *prompt_at {
                return None;
            }
            if kind == "turn.ended" {
                if event["turnId"].as_u64()? != *turn {
                    return None;
                }
            } else if event["promptId"].as_str()? != id {
                return None;
            }
            let completed = match string(&event, "reason") {
                "completed" => true,
                // These typed native outcomes finish no reply. They must not
                // poison a later, independently completed main prompt (e.g.
                // the successful recovery turn after a provider rate limit).
                "cancelled" | "failed" | "blocked" => false,
                _ => return None,
            };
            conflicted |= outcome.is_some_and(|previous| previous != completed);
            outcome = Some(completed);
            end = (completed && !conflicted).then_some(End {
                turn: *turn,
                prompt_at: *prompt_at,
                at,
            });
        }
    }
    end
}

fn eligible(record: &Value) -> Option<u64> {
    let clock = &record["kimi_turn_clock"];
    if string(record, "agent") != "kimi" || string(record, "run_id").is_empty()
        || string(record, "conversation_id").is_empty()
        || clock["run_id"] != record["run_id"] || clock["conversation_id"] != record["conversation_id"]
        || !["working", "tool"].contains(&string(record, "phase"))
        // A main end cannot erase outstanding tools or a pending interaction.
        || !record["active_tools"].as_object().is_some_and(|tools| tools.is_empty())
        || record.get("pending_approval").is_some_and(|pending| !pending.is_null())
    {
        return None;
    }
    let turn = clock["started"]
        .as_u64()
        .filter(|id| *id <= i64::MAX as u64)?;
    if clock.get("stopped").is_some_and(|value| {
        !value
            .as_u64()
            .is_some_and(|stopped| stopped <= i64::MAX as u64 && stopped < turn)
    }) {
        return None;
    }
    Some(turn)
}

fn not_after(record: &Value, key: &str, at: f64) -> bool {
    record.get(key).is_none_or(|value| {
        value.is_null()
            || value
                .as_f64()
                .is_some_and(|time| time.is_finite() && time >= 0.0 && time <= at)
    })
}

fn matches(record: &Value, end: &End) -> bool {
    eligible(record) == Some(end.turn)
        // A queued/new submission whose TurnStarted hook is still in flight
        // must not be settled by the preceding turn's terminal event.
        && not_after(record, "recovery_user_at", end.prompt_at)
        && not_after(record, "input_pending_at", end.prompt_at)
}

struct Proof {
    end: End,
    path: PathBuf,
    version: crate::platform::FileIdentity,
    len: u64,
}
impl Proof {
    fn current(&self) -> bool {
        crate::platform::FileIdentity::at(&self.path).is_ok_and(|(version, metadata)| {
            version == self.version && metadata.is_file() && metadata.len() == self.len
        })
    }
}

fn proof(record: &Value) -> Option<Proof> {
    eligible(record)?;
    let paths = questions::wire_paths(record).ok()?;
    let [path] = paths.as_slice() else {
        return None;
    };
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).ok()?;
    let (version, metadata) = crate::platform::FileIdentity::with_metadata(&file).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let len = metadata.len();
    let offset = len.saturating_sub(MAX_BYTES);
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 != len - offset {
        return None;
    }
    let end = terminal(&bytes, offset > 0)?;
    if !matches(record, &end) {
        return None;
    }
    let proof = Proof {
        end,
        path: path.clone(),
        version,
        len,
    };
    proof.current().then_some(proof)
}

// Called only before inspection/listing acquires Root. Generic record reads
// stay read-only. Re-read the exact run and wire version under Root, journal
// once, then persist; a real Stop racing either side converges on that row.
pub(super) fn reconcile(original: Value) -> Value {
    let Some(proof) = proof(&original) else {
        return original;
    };
    let settled = (|| -> Result<Option<Value>> {
        let _guard = lock(None)?;
        let Some(mut record) = read_run(string(&original, "name"), string(&original, "run_id"))?
        else {
            return Ok(None);
        };
        if record["conversation_id"] != original["conversation_id"]
            || record["agent_home"] != original["agent_home"]
            || record["session_id"] != original["session_id"]
            || !matches(&record, &proof.end)
            || !proof.current()
        {
            return Ok(None);
        }
        let mut event = json!({"hook_event_name":"Stop", "agent_id":"main",
            "turn_id":proof.end.turn,"turn_identity_source":"kimi_wire"});
        // A delayed inspection is not a new provider interaction. Never
        // renew cache/ping authority at observation time (or into the future).
        let completed_at = proof.end.at.min(now());
        keep_warm::observe(&mut record, &mut event, completed_at);
        journal::update_activity(&mut record, &event);
        keep_warm::settle(&mut record, &event, completed_at);
        if !proof.current() {
            return Ok(None);
        }
        journal::log_event(&record, &event)?;
        write(&mut record)?;
        Ok(Some(record))
    })();
    settled.ok().flatten().unwrap_or(original)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn prompt(turn: u64) -> Value {
        json!({"type":"turn.prompt","agentId":"main","turnId":turn,"promptId":format!("msg_{turn}"),"time":1000})
    }
    fn end(turn: u64) -> Value {
        json!({"type":"turn.ended","agentId":"main","turnId":turn,"reason":"completed","time":1100})
    }
    fn bytes(events: &[Value]) -> Vec<u8> {
        events
            .iter()
            .map(|event| format!("{event}\n"))
            .collect::<String>()
            .into_bytes()
    }
    fn record() -> Value {
        json!({"version":1,"agent":"kimi","name":"kimi/project/test","run_id":"run","conversation_id":"session_test",
            "activity":"busy","phase":"working","active_tools":{},"main_done":false,
            "kimi_turn_clock":{"run_id":"run","conversation_id":"session_test","started":0},
            "recovery_user_at":0.9,"input_pending_at":0.8})
    }
    #[test]
    fn only_typed_main_terminal_evidence_completes_the_exact_latest_turn() {
        let expected = Some(End {
            turn: 0,
            prompt_at: 1.0,
            at: 1.1,
        });
        assert_eq!(terminal(&bytes(&[prompt(0), end(0)]), false), expected);
        let completed = json!({"type":"prompt.completed","agentId":"main","promptId":"msg_0","reason":"completed","time":1100});
        assert_eq!(
            terminal(&bytes(&[prompt(0), completed.clone()]), false),
            expected
        );
        assert_eq!(
            terminal(&bytes(&[prompt(0), end(0), completed]), false),
            expected
        );
        for wrong in [
            json!({"type":"turn.ended","agentId":"child","turnId":0,"reason":"completed","time":1100}),
            json!({"type":"prompt.completed","agentId":"child","promptId":"msg_0","reason":"completed","time":1100}),
            json!({"type":"agent.message.appended","agentId":"main","text":"Done","time":1100}),
            json!({"type":"context.append_loop_event","agentId":"main","event":{"type":"step.end","finishReason":"end_turn"},"time":1100}),
            json!({"type":"turn.ended","turnId":0,"reason":"completed","time":1100}),
            json!({"type":"turn.ended","agentId":"main","turnId":1,"reason":"completed","time":1100}),
            json!({"type":"prompt.completed","agentId":"main","promptId":"msg_old","reason":"completed","time":1100}),
        ] {
            assert_eq!(terminal(&bytes(&[prompt(0), wrong]), false), None);
        }
        for key in ["reason", "turnId", "time"] {
            let mut wrong = end(0);
            wrong[key] = Value::Null;
            assert_eq!(terminal(&bytes(&[prompt(0), wrong]), false), None);
        }
        assert_eq!(
            terminal(&bytes(&[prompt(0), end(0), prompt(1)]), false),
            None
        );
        assert_eq!(
            terminal(&bytes(&[prompt(1), prompt(0), end(0)]), false),
            None
        );
        assert_eq!(terminal(&bytes(&[end(0)]), true), None);
        let mut clipped = b"clipped prefix\n".to_vec();
        clipped.extend(bytes(&[prompt(0), end(0)]));
        assert_eq!(terminal(&clipped, true), expected);
        for gap in [b"{broken}\n".as_slice(), b"{\"type\":\"gap\"}\n", b"[]\n"] {
            let mut broken = bytes(&[prompt(0), end(0)]);
            broken.extend(gap);
            assert_eq!(terminal(&broken, false), None);
        }
        let mut partial = bytes(&[prompt(0), end(0)]);
        partial.pop();
        assert_eq!(terminal(&partial, false), None);
    }
    #[test]
    fn an_older_native_failure_never_hides_the_successful_current_turn() {
        for reason in ["cancelled", "failed", "blocked"] {
            let mut failed = end(0);
            failed["reason"] = json!(reason);
            let finished = json!({"type":"prompt.completed","agentId":"main","promptId":"msg_0","reason":reason,"time":1100});
            assert_eq!(
                terminal(
                    &bytes(&[prompt(0), failed.clone(), finished.clone()]),
                    false
                ),
                None
            );
            let completed = json!({"type":"prompt.completed","agentId":"main","promptId":"msg_0","reason":"completed","time":1100});
            assert_eq!(
                terminal(&bytes(&[prompt(0), failed.clone(), completed]), false),
                None
            );
            assert_eq!(
                terminal(&bytes(&[prompt(0), end(0), finished.clone()]), false),
                None
            );
            assert_eq!(
                terminal(
                    &bytes(&[prompt(0), failed, finished, prompt(1), end(1)]),
                    false
                ),
                Some(End {
                    turn: 1,
                    prompt_at: 1.0,
                    at: 1.1
                })
            );
        }
        let mut malformed = end(0);
        malformed["reason"] = json!("unknown");
        assert_eq!(
            terminal(&bytes(&[prompt(0), malformed, prompt(1), end(1)]), false),
            None
        );
    }

    #[test]
    fn wire_completion_cannot_settle_another_run_new_input_or_outstanding_tools() {
        let evidence = terminal(&bytes(&[prompt(0), end(0)]), false).unwrap();
        assert!(matches(&record(), &evidence));
        for (key, value) in [
            ("run_id", json!("other")),
            ("conversation_id", json!("other")),
            ("agent", json!("claude")),
            ("phase", json!("approval")),
            ("active_tools", json!({"tool":{"name":"Shell"}})),
            ("pending_approval", json!({})),
            ("recovery_user_at", json!(1.01)),
            ("input_pending_at", json!(1.05)),
        ] {
            let mut wrong = record();
            wrong[key] = value;
            assert!(!matches(&wrong, &evidence), "{key}");
        }
        for (key, value) in [("started", 1), ("stopped", 0)] {
            let mut wrong = record();
            wrong["kimi_turn_clock"][key] = json!(value);
            assert!(!matches(&wrong, &evidence));
        }
        for invalid in [
            Value::Null,
            json!("0"),
            json!(false),
            json!(-1),
            json!({}),
            json!([]),
        ] {
            let mut wrong = record();
            wrong["kimi_turn_clock"]["stopped"] = invalid;
            assert!(!matches(&wrong, &evidence));
        }
        // Native main completion cannot finish an owned background child/group.
        let mut background = record();
        background["subagents"] = json!({"child":{"state":"working"}});
        background["subagent_groups"] = json!({"profile":{"active_count":1}});
        let event = json!({"hook_event_name":"Stop","agent_id":"main","turn_id":0});
        journal::update_activity(&mut background, &event);
        assert!(telemetry::work_remains(&background));
        assert!(!telemetry::settled(&background));
    }
    #[test]
    fn hook_silent_and_stop_plus_wire_converge_once_across_a_record_write_crash() {
        let private = tempfile::tempdir().unwrap();
        let private_path = crate::platform::canonical(private.path()).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &private_path)
            .env("HGS_STATE_DIR", private_path.join("state"))
            .env("HGS_CONFIG_DIR", private_path.join("config"))
            .env("ZERUS_KIMI_COMPLETION_TEST", "1")
            .args([
                "--exact",
                "state::kimi_completion::tests::private_completion_fixture",
                "--ignored",
                "--test-threads=1",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[test]
    #[ignore = "run by the private completion control"]
    fn private_completion_fixture() {
        assert_eq!(std::env::var("ZERUS_KIMI_COMPLETION_TEST").unwrap(), "1");
        let native = home().join(".kimi-code");
        let wire = native.join("sessions/workspace/session_test/agents/main/wire.jsonl");
        fs::create_dir_all(wire.parent().unwrap()).unwrap();
        for ordering in ["silent", "hook-first", "wire-first", "crash"] {
            let mut record = record();
            record["run_id"] = json!(ordering);
            record["kimi_turn_clock"]["run_id"] = json!(ordering);
            record["agent_home"] = json!(native);
            fs::write(&wire, bytes(&[prompt(0), end(0)])).unwrap();
            write(&mut record).unwrap();
            let hook = json!({"hook_event_name":"Stop","agent_id":"main","turn_id":0,"turn_identity_source":"kimi_hook"});
            if ordering == "hook-first" {
                journal::update_activity(&mut record, &hook);
                journal::log_event(&record, &hook).unwrap();
                write(&mut record).unwrap();
            } else if ordering == "crash" {
                // The journal committed, but the record write was lost.
                journal::log_event(&record, &hook).unwrap();
            }
            let settled = reconcile(record);
            assert!(telemetry::settled(&settled), "{ordering}: {settled}");
            if ordering != "hook-first" {
                // Observation can happen long after the native turn ended.
                // It must not grant a fresh keep-warm cache lifetime then.
                assert_eq!(settled["cache_touched"]["at"], json!(1.1));
                assert_eq!(settled["keep_warm_real_stop_at"], json!(1.1));
            }
            let again = reconcile(settled.clone());
            assert_eq!(again, settled);
            if ordering == "wire-first" {
                journal::log_event(&settled, &hook).unwrap();
            }
            let db = rusqlite::Connection::open(root().join("events.sqlite3")).unwrap();
            let count: u64 = db
                .query_row(
                    "SELECT COUNT(*) FROM events WHERE json_extract(payload, '$.type')='Stop'
                AND json_extract(payload, '$.run_id')=?",
                    [ordering],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "{ordering}");
            assert_eq!(read(string(&settled, "name")).unwrap()["main_done"], true);
        }
        let mut record = record();
        record["agent_home"] = json!(native);
        fs::write(&wire, bytes(&[prompt(0), end(0)])).unwrap();
        let original = proof(&record).unwrap();
        fs::write(&wire, bytes(&[prompt(0), end(0), prompt(1)])).unwrap();
        assert!(!original.current());
        assert!(proof(&record).is_none());
    }
}
