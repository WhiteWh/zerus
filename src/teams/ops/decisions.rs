use super::*;

pub(super) fn needs_merge(ctx: &Context<'_>, n: &Node) -> bool {
    ctx.team.settings.integration == Integration::Branch && n.kind != NodeKind::Stage
}
pub(super) fn accept(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::Accept { node } = ctx.op else {
        unreachable!()
    };
    ctx.active()?;
    ctx.undecided(*node)?;
    let n = ctx.live_node(*node)?;
    stage_end_authority(ctx, &n)?;
    if n.state != NodeState::Reported {
        return Err(invalid("Only a reported task can be accepted."));
    }
    pipeline::before_decision(ctx, *node)?;
    let decider = if is_person(ctx.actor) {
        Decider::Person
    } else {
        Decider::Lead
    };
    accept_result(ctx, &n, decider, false)
}
pub(super) fn accept_result(
    ctx: &mut Context<'_>,
    n: &Node,
    decider: Decider,
    owner_approved: bool,
) -> Result<()> {
    let node = n.id;
    let owner =
        !owner_approved && needs_merge(ctx, n) && !ctx.team.settings.delegations.merge_accepted;
    let seq = ctx.decision(
        node,
        "accept",
        "",
        if owner {
            DecisionState::AwaitingOwner
        } else {
            DecisionState::Ready
        },
    )?;
    ctx.db
        .execute(
            "UPDATE nodes SET decided_by=?3 WHERE team=?1 AND id=?2",
            params![ctx.team.id, node, encode(&decider)?],
        )
        .map_err(storage)?;
    if owner {
        ctx.reply.item = Some(talk::card(
            ctx,
            "integration",
            "Approve integration of the accepted result.",
            Some(node),
            Some(seq),
            &["approve", "reject"],
            &[],
        )?);
    }
    Ok(())
}
pub(super) fn return_task(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::Return { node, reason: why } = ctx.op else {
        unreachable!()
    };
    ctx.active()?;
    let earlier = ctx.supersede(*node)?;
    let mut n = ctx.live_node(*node)?;
    reason(why)?;
    let holder = handout::holder(ctx, *node)?;
    let Some(holder) = holder.filter(|_| n.state == NodeState::Reported) else {
        return Err(invalid(
            "Only a reported task with a holder can be returned.",
        ));
    };
    if members::removed_holder(ctx, *node)?.is_some() {
        return Err(invalid(
            "The task's holder was removed from the team; reassign or cancel the task instead.",
        ));
    }
    pipeline::override_task(ctx, *node)?;
    // A superseded Reassign did not happen: the work goes back to its holder,
    // who is also the member a later hand-out of the node follows.
    if earlier.is_some_and(|(_, kind)| kind == "reassign") {
        n.assignee = Some(holder);
        ctx.db
            .execute(
                "UPDATE nodes SET data=?3 WHERE team=?1 AND id=?2",
                params![ctx.team.id, node, rows::node_payload(&n)?],
            )
            .map_err(storage)?;
    }
    ctx.decision(*node, "return", why, DecisionState::Pending)?;
    Ok(())
}
pub(super) fn reassign(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::Reassign {
        node,
        member,
        reason: why,
    } = ctx.op
    else {
        unreachable!()
    };
    ctx.active()?;
    if ctx.node_table(*node)? == "node_archive" {
        let stage = ctx.node(*node)?;
        if stage.kind == NodeKind::Stage {
            reason(why)?;
            let m = ctx.member(member)?;
            replacement(ctx, &stage, &m)?;
            let replacement = pipeline::reassign_stopped_stage(ctx, &stage, member)?;
            ctx.decision(replacement, "reassign", why, DecisionState::Settled)?;
            ctx.reply.node = Some(replacement);
            return Ok(());
        }
    }
    ctx.supersede(*node)?;
    let mut n = ctx.live_node(*node)?;
    let m = ctx.member(member)?;
    reason(why)?;
    replacement(ctx, &n, &m)?;
    pipeline::override_task(ctx, *node)?;
    if n.kind == NodeKind::Task {
        if let Some(previous) = n.implementer.as_deref().filter(|p| *p != member) {
            super::handout::record_earlier(ctx, *node, previous)?;
        }
    }
    n.assignee = Some(member.clone());
    ctx.db
        .execute(
            "UPDATE nodes SET data=?3 WHERE team=?1 AND id=?2",
            params![ctx.team.id, node, rows::node_payload(&n)?],
        )
        .map_err(storage)?;
    // The hand-over route while the task may be in somebody's hands, a
    // person's taken-over run included; otherwise the ordinary hand-out
    // follows the new assignee.
    let state = if handout::in_hands(ctx, *node)? {
        DecisionState::Pending
    } else {
        DecisionState::Settled
    };
    ctx.decision(*node, "reassign", why, state)?;
    Ok(())
}
fn replacement(ctx: &Context<'_>, n: &Node, m: &Member) -> Result<()> {
    // A stage's replacement obeys the hand-out's rule too: a member who
    // implemented any round of the stage's target is refused with the
    // sentence (zerus-63rru.20 round 2).
    let stage_earlier = n.kind == NodeKind::Stage
        && (n.implementer.as_ref() == Some(&m.id)
            || match n.stage_of {
                Some(target) => rows::earlier_implementers(ctx.db, &ctx.team.id, target)
                    .map_err(storage)?
                    .iter()
                    .any(|id| id == &m.id),
                None => false,
            });
    if m.state != MemberState::Active
        || m.session.is_none()
        || m.id == ctx.team.lead
        || m.role != n.role
        || stage_earlier
    {
        return Err(invalid(
            "Replacement member cannot take this task: a stage is never handed to a member who implemented any round of the task.",
        ));
    }
    Ok(())
}
pub(super) fn cancel(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::Cancel { node, reason: why } = ctx.op else {
        unreachable!()
    };
    ctx.active()?;
    ctx.supersede(*node)?;
    let n = ctx.live_node(*node)?;
    stage_end_authority(ctx, &n)?;
    reason(why)?;
    pipeline::override_task(ctx, *node)?;
    ctx.decision(*node, "cancel", why, DecisionState::Pending)?;
    Ok(())
}
fn stage_end_authority(ctx: &Context<'_>, n: &Node) -> Result<()> {
    if n.kind == NodeKind::Stage && !is_person(ctx.actor) {
        return Err(Problem::new(
            code::NOT_ALLOWED,
            "The stage needs its holder's verdict; only the person may override it.",
        ));
    }
    Ok(())
}
pub(super) fn integration(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::RecordIntegration {
        node,
        commit,
        decision,
    } = ctx.op
    else {
        unreachable!()
    };
    let n = ctx.node(*node)?;
    let permit = integration::permit(ctx, *node, *decision, integration::FactKind::Merge)?;
    if !matches!(commit.len(), 40 | 64)
        || !commit
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(invalid(
            "Integration receipt must name a full lowercase commit ID.",
        ));
    }
    integration::record(ctx, &permit, *node, Some(commit), &[])?;
    if permit.late {
        // The supersede item was written before this late fact; say now that
        // the accepted result had already been merged, with the commit. Only
        // the item written for this fact's own acceptance: its generation
        // (zerus-63rru.37 round 4), while that acceptance is still the node's
        // latest (round 3). A supersede ends the node and every way back
        // advances its generation, so no two supersede items share one.
        ctx.db
            .execute(
                "UPDATE inbox SET text=replace(text, 'the person''s own run replaced the team''s accepted result, so nothing further will be merged. The task document was left unchanged.', ?3) WHERE team=?1 AND node=?2 AND kind='accepted_superseded' AND generation=?5 AND EXISTS(SELECT 1 FROM integration_facts f WHERE f.team=?1 AND f.node=?2 AND f.decision=?4 AND f.decision=(SELECT seq FROM decisions WHERE team=?1 AND node=?2 AND kind='accept' ORDER BY seq DESC LIMIT 1))",
                params![
                    ctx.team.id,
                    node,
                    format!("the accepted result had already been merged as {commit}; the person's own run continues the task. The task document was left unchanged."),
                    permit.decision,
                    permit.generation,
                ],
            )
            .map_err(storage)?;
    }
    if permit.provenance_only {
        rows::rejected_merge(
            ctx.db,
            &ctx.team.id,
            *node,
            permit.decision,
            &ctx.team.integration_branch,
            &encode(ctx.actor)?,
            ctx.now,
        )?;
    }
    integration::complete_merge(ctx, &permit)?;
    // The fact belongs to the accepted generation. A person's restore starts
    // a new round whose completion must not inherit this older merge.
    if permit.provenance_only
        || !needs_merge(ctx, &n)
        || n.integrated
        || permit.generation != ctx.generation(*node)?
    {
        return Ok(());
    }
    let table = ctx.node_table(*node)?;
    ctx.db
        .execute(
            &format!(
                "UPDATE {table} SET integrated=1,integration_commit=?3 WHERE team=?1 AND id=?2"
            ),
            params![ctx.team.id, node, commit],
        )
        .map_err(storage)?;
    let done: bool = ctx
        .db
        .query_row(
            &format!("SELECT task_stage='done' FROM {table} WHERE team=?1 AND id=?2"),
            params![ctx.team.id, node],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if done && !permit.late {
        ctx.settle(*node)?;
        ctx.close_inbox(*node)?;
    }
    Ok(())
}
pub(super) fn conflict(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::RecordIntegrationConflict {
        node,
        conflicts,
        decision,
    } = ctx.op
    else {
        unreachable!()
    };
    let n = ctx.node(*node)?;
    let permit = integration::permit(ctx, *node, *decision, integration::FactKind::Conflict)?;
    if conflicts.is_empty() || conflicts.len() > 200 {
        return Err(invalid(
            "No unresolved integration conflict can be recorded.",
        ));
    }
    for path in conflicts {
        reason(path)?;
        text(path, 4096)?;
    }
    let body = conflicts.join("\n");
    text(&body, 16_384)?;
    integration::record(ctx, &permit, *node, None, conflicts)?;
    if permit.late
        || !needs_merge(ctx, &n)
        || n.integrated
        || permit.generation != ctx.generation(*node)?
    {
        return Ok(());
    }
    ctx.settle(*node)?;
    // The conflict happened and stays recorded; a Return to a removed holder
    // could never be carried out, so the lead decides instead.
    if let Some(member) = members::removed_holder(ctx, *node)? {
        let text = format!(
            "Member {member} was removed from the team; the integration conflict of task {node} is recorded and nobody can take the task back, so it waits for the lead to reassign or cancel it.\n{body}"
        );
        let kind = format!("conflict_holder_removed:{decision}");
        ctx.reply.item = Some(members::hand_to_lead(ctx, *node, &kind, &text)?);
        return Ok(());
    }
    ctx.decision(*node, "return", &body, DecisionState::Pending)?;
    ctx.reply.item = Some(ctx.inbox(*node, "integration_conflict", &body)?);
    Ok(())
}
