//! Additive board metadata from one already bounded pinned team bundle.
use super::storage;
use crate::teams::store::{MemberRow, Page, QueueRow};
use serde_json::{json, Value};
use zerus_team_plan::{
    model::{NodeKind, Team},
    Problem,
};

pub(super) fn queue(team: &Team, page: Page<QueueRow>) -> Result<Page<Value>, Problem> {
    let rows = page
        .rows
        .into_iter()
        .map(|row| {
            let node = row.node;
            let mut value = serde_json::to_value(&node).map_err(storage)?;
            // What a view needs to foresee a refusal: who holds the attempt
            // and the kind of a pending decision, each null when none, and
            // whether a new Return, Reassign or Cancel would replace that
            // decision (always written; false when nothing is pending).
            value["holder"] = json!(row.holder);
            value["assigned_base"] = json!(row.assigned_base);
            value["pending_decision"] = json!(row.pending);
            value["pending_replaceable"] = json!(row.pending_replaceable);
            value["deps_editable"] = json!(row.deps_editable);
            // The lead's open item when no member can take the ready node:
            // its sentence, or null.
            value["unstaffed"] = json!(row.unstaffed);
            // zerus-3u5yu (5): a stage's target's earlier implementers, whom
            // the stage's Reassign refuses; the window leaves them out.
            if node.kind == NodeKind::Stage {
                value["earlier_implementers"] = json!(row.earlier_implementers);
            }
            // zerus-63rru.52 round 2: only a stopped or archived team's row
            // carries the stop outcome, as a stable code plus the store's
            // refusal reason as data, never a sentence.
            // zerus-63rru.52 round 4: a stopped or archived team without a
            // recorded stop sequence (a team stopped before the upgrade)
            // shows no outcome at all, exactly as before.
            if matches!(
                team.status,
                zerus_team_plan::model::TeamStatus::Stopped
                    | zerus_team_plan::model::TeamStatus::Archived
            ) && row.stop.as_ref().is_some_and(|f| f.stop_seq > 0)
            {
                if let Some(outcome) = stop_outcome(row.stop.as_ref()) {
                    value["stop_outcome"] = json!(outcome);
                }
            }
            value["kind"] = json!(if node.kind == NodeKind::Stage {
                "stage"
            } else {
                "task"
            });
            if let Some(pipeline) = team.bundle.pipelines.iter().find(|p| p.id == node.pipeline) {
                value["rounds"] = json!(pipeline.rounds());
                if node.kind == NodeKind::Stage {
                    if let (Some(target), Some(stage)) = (
                        node.stage_of,
                        pipeline.stages.get(node.stage_index as usize),
                    ) {
                        value["stage"] =
                            json!({"kind":stage.kind,"target":target,"round":node.round});
                    }
                }
            }
            Ok(value)
        })
        .collect::<Result<Vec<_>, Problem>>()?;
    Ok(Page {
        rows,
        next: page.next,
    })
}
/// The stop's outcome for one node, as the stable code plus the store's
/// refusal reason as data (zerus-63rru.52 round 2): the desktop maps the
/// code through the catalogs and shows the reason as secondary text; the
/// CLI renders its English sentence from the same code.
fn stop_outcome(facts: Option<&crate::teams::store::StopFacts>) -> Option<Value> {
    let facts = facts?;
    let code = match (facts.key.as_deref(), facts.cancel.as_deref(), facts.pending_cancel) {
        (Some("refused"), _, _) => "key_clear_refused",
        (Some("cleared"), _, _) => "key_cleared",
        (_, Some("refused"), _) => "cancel_refused",
        (_, Some("cancelled"), _) => "cancelled",
        (_, Some("released"), _) => "released",
        (None, None, true) => "pending",
        (None, None, false) => "unchanged",
        _ => "pending",
    };
    let reason = match code {
        "key_clear_refused" => facts.key_reason.clone(),
        "cancel_refused" => facts.cancel_reason.clone(),
        _ => None,
    };
    let mut outcome = json!({"code": code});
    if let Some(reason) = reason {
        outcome["reason"] = json!(reason);
    }
    Some(outcome)
}

/// The CLI's English sentence for an outcome code and its reason
/// (zerus-63rru.52 round 2); the desktop never renders this sentence. The
/// tests assert these words; the queue reply itself carries the code.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn stop_sentence(code: &str, reason: Option<&str>) -> String {
    match (code, reason) {
        ("key_clear_refused", Some(reason)) => {
            format!("The team stop's key clear was refused: {reason}")
        }
        ("key_cleared", _) => "The team stop cleared this task's key".to_owned(),
        ("cancel_refused", Some(reason)) => {
            format!("The team stop's cancel was refused: {reason}")
        }
        ("cancelled", _) => "The team stop cancelled this task".to_owned(),
        ("released", _) => "The team stop released this task".to_owned(),
        ("pending", _) => "The team stop has not reached this task yet".to_owned(),
        ("unchanged", _) => "The team stop left this finished task as it was".to_owned(),
        _ => format!("The team stop outcome {code}"),
    }
}

pub(super) fn members(
    team: &Team,
    members: Vec<MemberRow>,
    work: &std::collections::BTreeMap<String, usize>,
) -> Result<Vec<Value>, Problem> {
    members
        .into_iter()
        .map(|row| {
            let member = row.member;
            let mut value = serde_json::to_value(&member).map_err(storage)?;
            value["person_stopped"] = json!(row.person_stopped);
            value["open_work"] = json!(work.get(&member.id).copied().unwrap_or(0));
            let choices = if let Some(launch) = &member.launch {
                Some((
                    launch.model.as_str(),
                    launch.effort.as_str(),
                    &launch.account,
                ))
            } else if member.id == team.lead {
                let lead = &team.bundle.template.lead;
                Some((lead.model.as_str(), lead.effort.as_str(), &lead.account))
            } else {
                team.bundle
                    .roles
                    .iter()
                    .find(|r| r.id == member.role)
                    .map(|r| (r.model.as_str(), r.effort.as_str(), &r.account))
            };
            if let Some((model, effort, account)) = choices {
                if !model.trim().is_empty() {
                    value["model"] = json!(model);
                }
                value["effort"] = json!(effort);
                value["account_choice"] = json!(account);
            }
            Ok(value)
        })
        .collect()
}
