//! The evidence beside one task's decision (zerus-5gcbh), read on demand when
//! the Team tab's inspector shows a task: `hgs team evidence --team ID --node
//! N`. The task's latest report and its stages with each one's latest verdict,
//! validated against the asked team and task and bounded like the drawers.
use super::read::{valid_team, TeamError};
use super::view::{self, ServiceNote, NAME_CHARS, TEXT_CHARS};
use serde::Serialize;
use serde_json::Value;

/// The CLI's own stage bound for one read.
pub const MAX_STAGES: usize = 24;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TaskEvidence {
    pub team: String,
    pub node: u64,
    pub revision: u64,
    pub report: Option<ReportEvidence>,
    /// Newest first, as the store returns them.
    pub stages: Vec<StageEvidence>,
    pub more_stages: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ReportEvidence {
    pub generation: u64,
    /// The task's implementer; absent when the store names none.
    pub member: Option<String>,
    pub commit: String,
    pub text: String,
    pub text_truncated: bool,
    pub notes: Vec<ServiceNote>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct StageEvidence {
    pub node: u64,
    /// `review`, `test`, `land`, or absent when the pinned bundle has no such stage.
    pub kind: Option<String>,
    pub round: u64,
    pub state: String,
    pub verdict: Option<VerdictEvidence>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct VerdictEvidence {
    pub generation: u64,
    /// The stage holder who gave the verdict.
    pub member: Option<String>,
    pub pass: bool,
    pub text: String,
    pub text_truncated: bool,
}

fn invalid(message: &str) -> TeamError {
    TeamError::Invalid {
        message: message.into(),
    }
}

/// The command's words for one task: the team must be a canonical team ID and
/// the node a positive task number.
pub fn args(team: &str, node: u64) -> Result<Vec<String>, TeamError> {
    if !valid_team(team) {
        return Err(invalid("Not a team id."));
    }
    if node == 0 || node > u64::from(u32::MAX) {
        return Err(invalid("Not a task number."));
    }
    Ok([
        "team",
        "evidence",
        "--team",
        team,
        "--node",
        &node.to_string(),
        "--json",
    ]
    .map(str::to_owned)
    .to_vec())
}

fn count(value: &Value, what: &str) -> Result<u64, TeamError> {
    value
        .as_u64()
        .ok_or_else(|| invalid(&format!("The evidence gives no {what}.")))
}
fn optional_name(value: &Value, what: &str) -> Result<Option<String>, TeamError> {
    match value {
        Value::Null => Ok(None),
        Value::String(name) if !name.is_empty() => Ok(Some(view::cut(name, NAME_CHARS))),
        _ => Err(invalid(&format!("The evidence names an invalid {what}."))),
    }
}
fn text(value: &Value, what: &str) -> Result<(String, bool), TeamError> {
    let text = value
        .as_str()
        .ok_or_else(|| invalid(&format!("The evidence gives no {what} text.")))?;
    Ok((
        view::cut(text, TEXT_CHARS),
        text.chars().count() > TEXT_CHARS,
    ))
}

/// The reply of the asked task, or why it cannot be shown. A reply for another
/// team or task, more stages than the bound, or any malformed field is refused
/// whole; nothing is filled in.
pub fn project(reply: &Value, team: &str, node: u64) -> Result<TaskEvidence, TeamError> {
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(invalid("The evidence answer is not a success."));
    }
    if reply.get("team").and_then(Value::as_str) != Some(team)
        || reply.get("node").and_then(Value::as_u64) != Some(node)
    {
        return Err(invalid("The evidence names another team or task."));
    }
    let revision = count(&reply["revision"], "revision")?;
    let report = match reply.get("report") {
        Some(Value::Null) => None,
        Some(report @ Value::Object(_)) => {
            let (text, text_truncated) = text(&report["text"], "report")?;
            if !report["notes"].is_array() {
                return Err(invalid("The report gives no notes."));
            }
            Some(ReportEvidence {
                generation: count(&report["generation"], "report generation")?,
                member: optional_name(&report["member"], "member")?,
                commit: report["commit"]
                    .as_str()
                    .map(|commit| view::cut(commit, NAME_CHARS))
                    .ok_or_else(|| invalid("The report gives no commit."))?,
                text,
                text_truncated,
                notes: view::service_notes(report),
            })
        }
        _ => return Err(invalid("The evidence gives no report field.")),
    };
    let rows = reply["stages"]
        .as_array()
        .ok_or_else(|| invalid("The evidence gives no stages."))?;
    if rows.len() > MAX_STAGES {
        return Err(invalid("The evidence exceeds its stage bound."));
    }
    let stages = rows
        .iter()
        .map(|stage| {
            let verdict = match &stage["verdict"] {
                Value::Null => None,
                verdict @ Value::Object(_) => {
                    let (text, text_truncated) = text(&verdict["text"], "verdict")?;
                    Some(VerdictEvidence {
                        generation: count(&verdict["generation"], "verdict generation")?,
                        member: optional_name(&verdict["member"], "reviewer")?,
                        pass: verdict["pass"]
                            .as_bool()
                            .ok_or_else(|| invalid("The verdict gives no pass or return."))?,
                        text,
                        text_truncated,
                    })
                }
                _ => return Err(invalid("The stage gives no verdict field.")),
            };
            Ok(StageEvidence {
                node: count(&stage["node"], "stage")?,
                kind: optional_name(&stage["kind"], "stage kind")?,
                round: count(&stage["round"], "round")?,
                state: stage["state"]
                    .as_str()
                    .map(|state| view::cut(state, NAME_CHARS))
                    .ok_or_else(|| invalid("The stage gives no state."))?,
                verdict,
            })
        })
        .collect::<Result<Vec<_>, TeamError>>()?;
    Ok(TaskEvidence {
        team: team.to_owned(),
        node,
        revision,
        report,
        stages,
        more_stages: reply["more_stages"]
            .as_bool()
            .ok_or_else(|| invalid("The evidence gives no stage bound flag."))?,
    })
}

#[cfg(test)]
#[path = "evidence/tests.rs"]
mod tests;
