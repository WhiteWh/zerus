use super::*;
pub(super) fn kind(operation: &Operation) -> Result<String, Problem> {
    Ok(serde_json::to_value(operation).map_err(storage)?["op"]
        .as_str()
        .ok_or_else(|| invalid("Missing operation kind."))?
        .to_owned())
}

pub(super) fn references(request: &Request) -> Result<Vec<String>, Problem> {
    let value = serde_json::to_value(&request.operation).map_err(storage)?;
    let mut refs = vec![format!("team:{}", request.team)];
    for (field, prefix) in [
        ("node", "node"),
        ("phase", "phase"),
        ("member", "member"),
        ("effect_request_id", "effect"),
        ("item", "inbox"),
        ("card", "card"),
    ] {
        if let Some(value) = value.get(field) {
            if let Some(value) = value.as_str() {
                refs.push(format!("{prefix}:{value}"));
            } else if let Some(value) = value.as_u64() {
                refs.push(format!("{prefix}:{value}"));
            }
        }
    }
    if let Some(task) = value
        .get("task")
        .and_then(|t| t.get("uuid"))
        .and_then(|id| id.as_str())
    {
        refs.push(format!("task:{task}"));
    }
    for field in ["scope", "relative_to"] {
        if let Some(scope) = value.get(field) {
            if let Some(phase) = scope.get("phase").and_then(|v| v.as_u64()) {
                refs.push(format!("phase:{phase}"));
            }
            if let Some(id) = scope.get("publication").and_then(|v| v.as_str()) {
                refs.push(format!("publication:{id}"));
            }
            if let Some(id) = scope
                .get("rework")
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
            {
                refs.push(format!("rework:{id}"));
            }
        }
    }
    if let Some(pause) = value.get("pause").and_then(|v| v.as_str()) {
        refs.push(format!("pause:{pause}"));
    }
    Ok(refs)
}

pub(super) fn append(ctx: &Context<'_>, request: &Request) -> Result<u64> {
    let kind = ctx
        .event
        .map(str::to_owned)
        .map_or_else(|| kind(ctx.op), Ok)?;
    let mut body = match ctx.op {
        Operation::AdvanceBase { commit } => {
            let reference: String = ctx
                .db
                .query_row(
                    "SELECT main_ref FROM base_advances WHERE team=?1 AND revision=?2",
                    params![ctx.team.id, ctx.reply.revision],
                    |r| r.get(0),
                )
                .map_err(storage)?;
            format!("Base advanced from {} to {commit} via {reference}. Earlier hand-outs keep their assigned base.",ctx.team.base_commit)
        }
        Operation::CloseCard { reason, .. } => reason.clone(),
        Operation::ObserveTask { node, observation }
            if ctx.event == Some("person_override") && observation.key != KeyState::Ours =>
        {
            format!("Task {node} left the team.")
        }
        Operation::ObserveTask { node, .. } if ctx.event == Some("accepted_superseded") => {
            format!("The person's own run replaced the accepted result of task {node}; the accepted decision was superseded and nothing was merged.")
        }
        // What the hand-out came to, and a refusal's reason, in the row's
        // own text (zerus-63rru.79): an intent that typed nothing must not
        // read like a hand-out that happened.
        Operation::HandoutOutcome { outcome, .. } => format!(
            "{kind} at revision {}: {}",
            ctx.reply.revision,
            outcome_text(outcome)
        ),
        _ => format!("{kind} at revision {}", ctx.reply.revision),
    };
    if let Some(node) = ctx.land_skipped_for {
        body.push_str(&format!(
            ": Land stages skipped for task #{node} because landing is disabled in Team settings."
        ));
    }
    let mut refs = references(request)?;
    if let Some(card) = ctx.closed_confirmation_card {
        body.push_str(". ");
        body.push_str(CONFIRMATION_CLOSED);
        refs.push(format!("card:{card}"));
    }
    if let Some(member) = &ctx.reply.member {
        let reference = format!("member:{member}");
        if !refs.contains(&reference) {
            refs.push(reference);
        }
    }
    let replacement_node = match ctx.op {
        Operation::Reassign { node, .. } => ctx.reply.node.filter(|id| id != node),
        _ => None,
    };
    if let Some(replacement) = replacement_node {
        refs.push(format!("node:{replacement}"));
    }
    if let Some(earlier) = ctx.superseded {
        let (old, new) = replacement(ctx, earlier)?;
        body.push_str(&format!(
            "; decision {earlier} ({old}) superseded by decision {} ({})",
            new.0, new.1
        ));
        refs.push(format!("decision:{earlier}"));
        refs.push(format!("decision:{}", new.0));
    }
    ctx.db.execute("INSERT INTO journal(team,at_ms,actor,kind,request_id,text,refs) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![ctx.team.id,ctx.now,encode(ctx.actor)?,kind,request.request_id,body,encode(&refs)?]).map_err(storage)?;
    let source_seq = ctx.db.last_insert_rowid() as u64;
    let cutoff: Option<u64> = ctx
        .db
        .query_row(
            "SELECT seq FROM journal WHERE team=?1 ORDER BY seq DESC LIMIT 1 OFFSET ?2",
            params![ctx.team.id, store::LIVE_JOURNAL_CAP],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    if let Some(cutoff) = cutoff {
        archive(ctx, Some(cutoff))?;
    }
    if matches!(ctx.op, Operation::Archive)
        || matches!(ctx.team.status, TeamStatus::Archived | TeamStatus::Finished)
    {
        archive(ctx, None)?;
    }
    Ok(source_seq)
}
/// A hand-out's outcome as the journal says it: its name, then its reason,
/// cleaned as outside text is where the service takes it
/// (`teams::clean::sentence`, zerus-63rru.59), when it has one.
fn outcome_text(outcome: &HandoutOutcome) -> String {
    let (name, reason) = match outcome {
        HandoutOutcome::Submitted { .. } => ("submitted", ""),
        HandoutOutcome::Refused { reason } => ("refused", reason.as_str()),
        HandoutOutcome::Uncertain { reason } => ("uncertain", reason.as_str()),
        HandoutOutcome::CuttingOver { reason } => ("cutting over", reason.as_str()),
        HandoutOutcome::HandedBack { reason } => ("handed back", reason.as_str()),
    };
    let reason = crate::teams::clean::sentence(reason, 512);
    if reason.is_empty() {
        name.to_owned()
    } else {
        format!("{name}: {reason}")
    }
}
/// The superseded decision's kind, and the decision that replaced it: the
/// node's newest, recorded by this operation.
fn replacement(ctx: &Context<'_>, earlier: u64) -> Result<(String, (u64, String))> {
    let (node, kind): (u32, String) = ctx
        .db
        .query_row(
            "SELECT node,kind FROM decisions WHERE seq=?1 AND team=?2",
            params![earlier, ctx.team.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(storage)?;
    let new = ctx
        .db
        .query_row(
            "SELECT seq,kind FROM decisions WHERE team=?1 AND node=?2 ORDER BY seq DESC LIMIT 1",
            params![ctx.team.id, node],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(storage)?;
    Ok((kind, new))
}
fn archive(ctx: &Context<'_>, cutoff: Option<u64>) -> Result<()> {
    ctx.db.execute("INSERT INTO journal_archive SELECT * FROM journal WHERE team=?1 AND (?2 IS NULL OR seq<=?2)",params![ctx.team.id,cutoff]).map_err(storage)?;
    ctx.db
        .execute(
            "DELETE FROM journal WHERE team=?1 AND (?2 IS NULL OR seq<=?2)",
            params![ctx.team.id, cutoff],
        )
        .map_err(storage)?;
    Ok(())
}
