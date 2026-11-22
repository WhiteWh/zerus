//! The first terminal send is written before Codex assigns its conversation.
//! Keep that receipt immutable and prove the binding from this run's journal.
use super::*;

pub(super) fn receipt(record: &Value, receipt: &Value) -> bool {
    record["agent"] == "codex"
        && receipt["first_message"] == true
        && receipt["conversation_id"].as_str() == Some("")
        && receipt["transport"] == "terminal"
        && codex_tasks::bootstrap_start(record)
}

pub(super) fn hook(record: &Value, receipt: &Value, event: &Value) -> bool {
    self::receipt(record, receipt)
        && string(event, "agent_id").is_empty()
        && matches!(
            string(event, "hook_event_name"),
            "UserPromptSubmit" | "UserPromptQueued" | "TurnStarted" | "TaskStarted"
        )
        && hook_text("codex", event) == receipt["submitted_text"].as_str()
}
