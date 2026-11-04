//! Qt AutomaticApprovals: verified one-time permissions only. This module owns
//! no journal or native writer: the existing approval-log and Actions lane do.
use crate::{actions::Target, questions::AnswerItem, ViewRow};
use serde_json::{json, Value};

pub const ENABLED: &str = "automaticApprovals/enabled";
pub const ERROR: &str = "automaticApprovals/error";
pub const CANCELLED: &str =
    "Automatic approvals were disabled or the session changed before delivery.";
pub const INVALID: &str =
    "The approval journal did not confirm a durable request. No answer was sent.";
pub const MAX_QUESTION: usize = 120 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub target: Target,
    pub provider: String,
    pub key: String,
    pub created: i64,
    pub catalog_key: String,
    pub machine_id: String,
}
pub fn candidate(computer: &str, row: &ViewRow) -> Option<Candidate> {
    let provider = row.notify.cmd.as_str();
    if !row.tracked
        || row.state != "running"
        || row.process_state == "exited"
        || !matches!(provider, "claude" | "opencode" | "dsh")
    {
        return None;
    }
    let permission = row.phase.as_deref() == Some("approval")
        || (matches!(provider, "opencode" | "dsh") && row.phase.as_deref() == Some("input"))
        || row.subagents.values().any(|child| {
            child["phase"] == "approval"
                || child["state"] == "approval"
                || child["display_state"] == "approval"
                || child["status"] == "Needs approval"
        });
    let run = row.run_id.as_deref().filter(|s| !s.is_empty())?;
    let conversation = row.conversation_id.as_deref().filter(|s| !s.is_empty())?;
    if !permission
        || row.name.is_empty()
        || row.name.len() > 1024
        || run.len() > 1024
        || conversation.len() > 1024
    {
        return None;
    }
    Some(Candidate {
        target: Target {
            computer: computer.into(),
            name: row.name.clone(),
            run_id: run.into(),
            conversation_id: conversation.into(),
        },
        provider: provider.into(),
        key: format!("{computer}\n{}", row.name),
        created: row.created,
        catalog_key: String::new(),
        machine_id: String::new(),
    })
}
pub fn same(target: &Candidate, row: &ViewRow) -> bool {
    row.name == target.target.name
        && row.created == target.created
        && row.state == "running"
        && row.tracked
        && row.process_state != "exited"
        && row.notify.cmd == target.provider
        && row.run_id.as_deref() == Some(&target.target.run_id)
        && row.conversation_id.as_deref() == Some(&target.target.conversation_id)
}

/// Same exact grant selectors as approval_log::answers. A journal response
/// cannot broaden them; the reserved answers must equal this local selection.
pub fn once(provider: &str, question: &Value) -> Option<Vec<AnswerItem>> {
    if question["approval"] != true
        || question["can_answer"] != true
        || question["trust_request"] == true
    {
        return None;
    }
    let source = question["source"].as_str()?;
    if !matches!(
        (provider, source),
        ("claude", "claude_tool_approval")
            | ("opencode", "opencode_native")
            | ("dsh", "dsh_native")
    ) {
        return None;
    }
    for key in ["question_id", "run_id", "conversation_id"] {
        if !question[key]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control))
        {
            return None;
        }
    }
    let hash = question["question_hash"].as_str()?;
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let items = question["questions"].as_array()?;
    let hook = source == "claude_tool_approval"
        && question["answer_transport"] == "claude_hook"
        && question["panel_verified"] == true;
    if items.len() != 1
        || items[0]["multi_select"] == true
        || items[0]["allow_other"] == true && !hook
    {
        return None;
    }
    let item = &items[0];
    let options = item["options"].as_array()?;
    if options.len() > 64 {
        return None;
    }
    let grants: Vec<_> = options
        .iter()
        .filter(|option| match source {
            "claude_tool_approval" => {
                (question["answer_transport"] == "claude_tui" || hook)
                    && question["approval_choices"] == true
                    && item["id"] == "approval"
                    && option["scope"] == "once"
                    && matches!(
                        option["label"].as_str(),
                        Some("Yes" | "Yes, but ask again next time")
                    )
            }
            "opencode_native" => {
                question["answer_transport"] == "native_opencode"
                    && question["kind"] == "opencode_permission"
                    && item["id"] == "permission"
                    && option["id"] == "once"
            }
            "dsh_native" => {
                question["answer_transport"] == "native_dsh"
                    && item["id"] == "decision"
                    && option["id"] == "allow"
                    && option["label"] == "Allow once"
            }
            _ => false,
        })
        .collect();
    if grants.len() != 1 {
        return None;
    }
    let id = grants[0]["id"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control))?;
    if options.iter().filter(|option| option["id"] == id).count() != 1 {
        return None;
    }
    let item_id = item["id"].as_str()?;
    Some(vec![AnswerItem::Choice {
        question_id: item_id.into(),
        selected_option_ids: vec![id.into()],
        text: String::new(),
    }])
}
pub fn question(target: &Candidate, details: &Value) -> Option<Value> {
    if details["runtime_state"] != "live"
        || details["process_state"] != "running"
        || details["run_id"] != target.target.run_id
        || details["conversation_id"] != target.target.conversation_id
    {
        return None;
    }
    for question in details["pending_questions"].as_array()?.iter().take(64) {
        if question["run_id"] != target.target.run_id
            || question["conversation_id"] != target.target.conversation_id
        {
            continue;
        }
        let mut bytes = crate::fsx::checkpoint::Bounded {
            bytes: Vec::new(),
            limit: MAX_QUESTION,
        };
        if once(&target.provider, question).is_some()
            && serde_json::to_writer(&mut bytes, question).is_ok()
        {
            return Some(question.clone());
        }
    }
    None
}
pub struct Reservation {
    pub id: i64,
    pub request: String,
    pub answers: Vec<AnswerItem>,
}
/// None means an existing/ineligible record: it never permits another send.
pub fn reserved(
    reply: &Value,
    provider: &str,
    question: &Value,
) -> Result<Option<Reservation>, &'static str> {
    if reply["eligible"] != true || reply["fresh"] != true {
        return Ok(None);
    }
    let id = reply["id"].as_i64().filter(|id| *id > 0).ok_or(INVALID)?;
    let request = reply["request_id"]
        .as_str()
        .filter(|s| crate::creation::canonical_uuid(s))
        .ok_or(INVALID)?;
    let answers = once(provider, question).ok_or(INVALID)?;
    if reply["answers"] != json!(answers) {
        return Err(INVALID);
    }
    Ok(Some(Reservation {
        id,
        request: request.into(),
        answers,
    }))
}

#[cfg(test)]
mod tests;
