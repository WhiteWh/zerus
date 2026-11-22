use super::*;
use serde::{Deserialize, Serialize};
use std::path::Component;
pub(super) const SEED_BYTES: usize = 256 << 10;
const FIELD_BYTES: usize = 16 << 10;
// V1 could pair a writer's projection with a different writer's file stamp.
// Never reuse those rows: bounded repair must observe the canonical bytes.
const SEED_VERSION: u64 = 4;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Key(pub String);
impl Key {
    pub fn at(root: &Path, path: &Path) -> Option<Self> {
        let relative = path.strip_prefix(root).ok()?;
        if relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return None;
        }
        let folder = relative.parent()?.to_str()?.replace('\\', "/");
        if !storage::BINDING_DIRS.contains(&folder.as_str()) {
            return None;
        }
        let stem = relative.file_stem()?.to_str()?;
        if relative.extension()? != "json" {
            return None;
        }
        let valid = if folder.ends_with("sessions") {
            session_id::canonical(stem)
        } else {
            stem.len() == 64
                && stem
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        valid.then(|| Self(relative.to_str().unwrap().replace('\\', "/")))
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            Self::at(Path::new(""), Path::new(&self.0)).as_ref() == Some(self),
            "Invalid display source"
        );
        Ok(())
    }
    pub fn matches(&self, record: &Value) -> bool {
        let path = Path::new(&self.0);
        let id = path.file_stem().and_then(|v| v.to_str()).unwrap_or("");
        let native = if self.0.starts_with("dsh/") {
            Some("dsh")
        } else if self.0.starts_with("opencode/") {
            Some("opencode")
        } else {
            None
        };
        let schema = match native {
            Some(agent) => {
                record["backend"] == agent
                    && string(record, "name").starts_with(&format!("{agent}/"))
                    && if agent == "dsh" {
                        string(record, "conversation_id").starts_with("session-")
                            && string(record, "conversation_id").len() <= 2048
                    } else {
                        opencode::native_id(string(record, "conversation_id"), "ses_")
                    }
            }
            None => record["version"] == 1 && AGENTS.contains(&string(record, "agent")),
        };
        schema
            && !string(record, "name").is_empty()
            && if self.0.contains("sessions/") {
                storage::is_id_layout_record(record, id)
            } else {
                !storage::in_id_layout(record) && storage::label_key(string(record, "name")) == id
            }
    }
}
/// Pure, bounded public inputs. journal::summary does native/journal I/O and
/// must not be called here. No shell output, transcript, consent or tool args.
pub(super) struct Seed;
impl Seed {
    pub fn encode(key: &Key, record: &Value) -> anyhow::Result<String> {
        anyhow::ensure!(
            key.matches(record),
            "Display record identity/layout mismatch"
        );
        anyhow::ensure!(
            record.get("layout_transition").is_none(),
            "Legacy identity transition requires canonical reconciliation"
        );
        let mut out = serde_json::Map::new();
        for key in [
            "version",
            "layout",
            "name",
            "journal_name",
            "session_id",
            "run_id",
            "conversation_id",
            "launch_id",
            "launch_label",
            "terminal_name",
            "pane",
            "agent",
            "backend",
            "activity",
            "phase",
            "state",
            "conversation_state",
            "created",
            "updated",
            "pid",
            "process_start",
            "supervisor_pid",
            "supervisor_start",
            "cwd",
            "launch_dir",
            "model",
            "effort",
            "last_event_at",
            "recovery_user_at",
            "recovery_success_at",
            "recovery_cancel_at",
            "keep_warm_prompt_at",
            "turn_started",
            "last_main_progress_at",
            "compaction_started",
            "attention_started",
            "main_done",
            "paused",
            "starting",
            "clearing",
            "resuming",
            "rename_shadow",
            "rename_target",
            "kimi_native_roster",
            "subagent_groups_complete",
            "subagents_completed_pruned",
            "subagents_ended_pruned",
            "attention_id",
            "reply_id",
            "last_reply_at",
            "last_error",
            "completion",
            "startup_kind",
            "cache_hint",
            "context_window",
            "context_tokens",
            "context_percent",
            "goal",
            "goal_progress",
            "account_id",
            "agent_home",
            "supervisor",
            "hgs",
            "expected_id",
            "requested_id",
            "run_identity_version",
            "error",
            "error_kind",
            "provider_error",
            "transcript",
            "model_source_path",
            "model_source_offset",
            "effort_source_path",
            "effort_source_offset",
            "fork_parent_id",
            "fork_source_name",
            "completion_source",
            "completion_reason",
            "exit_code",
            "termination_signal",
            "resume_pending",
            "cleared_conversations",
            "service",
            "instance_id",
            "permissions_pending",
            "permission_mode",
        ] {
            if let Some(value) = record.get(key) {
                bounded(value)?;
                out.insert(key.into(), value.clone());
            }
        }
        // Public availability depends on the presence of launch arguments,
        // never on a cached recipe granting authority. Do not retain argv.
        if record["base"].is_array() {
            out.insert("base".into(), json!([]));
        }
        // This legacy public field is complete, never clipped into a different
        // request. Large values require the original bounded canonical overlay.
        let recovery_needs_canonical = if let Some(value) = record.get("recovery_user_text") {
            let fits = match value.as_str() {
                Some(text) => text.len() <= SEED_BYTES / 2,
                None => bounded(value).is_ok(),
            };
            if fits {
                out.insert("recovery_user_text".into(), value.clone());
            }
            !fits
        } else {
            false
        };
        let private_transition = [
            "pausing",
            "terminating",
            "original_owner",
            "clear_context_request",
            "restore_archive_cleanup",
            "restore_archive_clear_source",
            "restore_archive_id",
        ]
        .iter()
        .any(|key| record.get(*key).is_some());
        out.insert("_listing".into(),json!({
            // This is a public display classification, not native/action proof.
            // Unknown and private shapes retain the original canonical path.
            "live_public":matches!(string(record,"agent"),"claude"|"codex")
                && matches!(string(record,"phase"),"idle"|"working"|"tool")
                && matches!(string(record,"activity"),"idle"|"busy")
                && !string(record,"conversation_id").is_empty()
                && string(record,"expected_id").is_empty()
                && string(record,"error").is_empty()
                && ["expected_id","error"].iter().all(|key|record[*key].is_null()||record[*key].is_string())
                && ["paused","starting","clearing","resuming"].iter()
                    .all(|key|record[*key].is_null() || record[*key]==false)
                && !recovery_needs_canonical && !private_transition && !resume_input::pending(record) && !resume_input::fork_pending(record)
                && !["pending_approval","pending_approval_at","pending_child_approvals","child_approval_overflow","exited_at","session_end","permissions_pending","startup_choice"]
                    .iter().any(|key|record.get(*key).is_some())
                && (record["pending_questions"].is_null() || record["pending_questions"].as_object().is_some_and(|questions|questions.is_empty()))
                && (record["subagents"].is_null() || record["subagents"].as_object().is_some_and(|children|children.is_empty())),
            "requires_canonical":recovery_needs_canonical || private_transition || resume_input::pending(record) || resume_input::fork_pending(record)
                || record["agent"]=="kimi" || record["subagents"].as_object().is_some_and(|children|children.values().any(|child| ["working","finished"].contains(&string(child,"state")))),
            "system_prompt":journal::system_prompt(string(record,"prompt")).is_some(),
        }));
        for (key, fields) in [
            ("pending_settings", &["request_id", "model", "effort"][..]),
            (
                "session_end",
                &["at", "run_id", "conversation_id", "reason"][..],
            ),
        ] {
            if record.get(key).is_some() {
                out.insert(key.into(), selected(&record[key], fields)?);
            }
        }
        for key in ["prompt", "last_message"] {
            if record.get(key).is_some() {
                out.insert(key.into(), json!(journal::clipped(&record[key], 240)));
            }
        }
        if record["keep_warm_quiet"].is_object() {
            out.insert(
                "keep_warm_quiet".into(),
                json!({
            "prompt":journal::clipped(&record["keep_warm_quiet"]["prompt"],240),
            "last_message":journal::clipped(&record["keep_warm_quiet"]["last_message"],240)}),
            );
        }
        for (key, fields) in [
            ("worktree_owner", &["id"][..]),
            ("account_binding", &["id", "home", "agent"][..]),
            (
                "parent",
                &[
                    "schema",
                    "machine",
                    "host",
                    "name",
                    "session_id",
                    "run_id",
                    "conversation_id",
                    "source",
                    "at",
                ][..],
            ),
        ] {
            if record[key].is_object() {
                out.insert(key.into(), selected(&record[key], fields)?);
            }
        }
        let mut child_detail_needs_canonical = false;
        for (key, fields) in [
            ("active_tools", &["name", "detail", "started"][..]),
            (
                "subagents",
                &[
                    "id",
                    "name",
                    "state",
                    "display_state",
                    "provider",
                    "external",
                    "announced",
                    "model",
                    "reply_id",
                    "native_roster",
                    "revived_at",
                    "last_event_at",
                    "started",
                    "ended",
                    "current_tool",
                    "tool_detail",
                    "prompt",
                    "description",
                    "attention_id",
                    "detail",
                    "updated",
                    "completion_source",
                    "tracking_evidence",
                    "process_state",
                ][..],
            ),
            (
                "subagent_groups",
                &[
                    "active_count",
                    "total_count",
                    "completed_count",
                    "unknown_count",
                    "name",
                    "profile",
                    "started",
                    "label",
                    "detail",
                    "updated",
                ][..],
            ),
        ] {
            if let Some(values) = record[key].as_object() {
                anyhow::ensure!(
                    values.len() <= 128,
                    "Display child/tool set exceeds its bound"
                );
                let mut result = serde_json::Map::new();
                for (id, value) in values {
                    anyhow::ensure!(id.len() <= 2048, "Display identity exceeds its bound");
                    let child = if key == "subagents"
                        && value["detail"]
                            .as_str()
                            .is_some_and(|text| text.len() > FIELD_BYTES)
                    {
                        // Native replies are bounded in characters and may be
                        // larger than this compact field's byte bound. Retain
                        // the canonical text; every listing path must read it
                        // before this incomplete hint can become a public row.
                        child_detail_needs_canonical = true;
                        let fields = fields
                            .iter()
                            .copied()
                            .filter(|field| *field != "detail")
                            .collect::<Vec<_>>();
                        selected(value, &fields)?
                    } else {
                        selected(value, fields)?
                    };
                    result.insert(id.clone(), child);
                }
                out.insert(key.into(), Value::Object(result));
            }
        }
        if child_detail_needs_canonical {
            let classification = out.get_mut("_listing").expect("projector classification");
            classification["requires_canonical"] = json!(true);
            classification["live_public"] = json!(false);
        }
        let mut public = Value::Object(out);
        // These two legacy repairs are pure and bounded by the compact roster.
        // Native child/process reconciliation requires the selected canonical
        // overlay; it is never performed by a publisher or from cached pins.
        journal::repair_kimi_main(&mut public);
        journal::repair_claude_internal(&mut public);
        let digest = storage::label_key(&serde_json::to_string(&public)?);
        let encoded = serde_json::to_string(
            &json!({"version":SEED_VERSION,"record":public,"digest":digest}),
        )?;
        anyhow::ensure!(
            encoded.len() <= SEED_BYTES,
            "Display seed exceeds its bound"
        );
        Ok(encoded)
    }
    pub fn decode(key: &Key, encoded: &str) -> anyhow::Result<Value> {
        anyhow::ensure!(
            encoded.len() <= SEED_BYTES,
            "Display seed exceeds its bound"
        );
        key.validate()?;
        let value: Value = serde_json::from_str(encoded)?;
        anyhow::ensure!(
            value["version"] == SEED_VERSION && key.matches(&value["record"]),
            "Display seed identity changed"
        );
        let digest = storage::label_key(&serde_json::to_string(&value["record"])?);
        anyhow::ensure!(
            value["digest"].as_str() == Some(digest.as_str()),
            "Display seed checksum mismatch"
        );
        Ok(value)
    }
}
fn selected(value: &Value, fields: &[&str]) -> anyhow::Result<Value> {
    let mut result = serde_json::Map::new();
    for field in fields {
        if let Some(value) = value.get(*field) {
            bounded(value)?;
            result.insert((*field).into(), value.clone());
        }
    }
    Ok(Value::Object(result))
}
fn bounded(value: &Value) -> anyhow::Result<()> {
    fn visit(v: &Value, bytes: &mut usize, nodes: &mut usize, depth: usize) -> anyhow::Result<()> {
        anyhow::ensure!(
            depth <= 8 && *nodes > 0,
            "Display field exceeds its structural bound"
        );
        *nodes -= 1;
        let length = match v {
            Value::String(s) => s.len(),
            Value::Array(a) => {
                for v in a {
                    visit(v, bytes, nodes, depth + 1)?;
                }
                0
            }
            Value::Object(o) => {
                for (k, v) in o {
                    anyhow::ensure!(k.len() <= *bytes, "Display field exceeds its byte bound");
                    *bytes -= k.len();
                    visit(v, bytes, nodes, depth + 1)?;
                }
                0
            }
            _ => 8,
        };
        anyhow::ensure!(length <= *bytes, "Display field exceeds its byte bound");
        *bytes -= length;
        Ok(())
    }
    let mut bytes = FIELD_BYTES;
    visit(value, &mut bytes, &mut 256, 0)
}
