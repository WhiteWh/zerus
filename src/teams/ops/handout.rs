use super::*;

pub(super) struct Effect {
    pub node: u32,
    pub member: String,
    pub generation: u32,
    pub kind: String,
    pub state: String,
    pub outcome: Option<String>,
}
/// Who holds the node's attempt, by the store's one rule (the attempt
/// holder that the queue row shows): a hand-out that was only reserved, or
/// was refused or superseded, holds nothing.
pub(super) fn holder(ctx: &Context<'_>, node: u32) -> Result<Option<String>> {
    Ok(crate::teams::store::holder_of(ctx.db, &ctx.team.id, node)?.map(|h| h.member))
}
/// The member the newest effect of the current generation went to, unless
/// that effect is known refused or superseded (the store's `NOT_RECEIVED`).
/// A member's own report or question proves the delivery and may arrive
/// before the hand-out's outcome is recorded, so it is admitted against
/// this, not against the holder: the two differ in a reserved effect alone.
pub(super) fn recipient(ctx: &Context<'_>, node: u32) -> Result<Option<String>> {
    ctx.db.query_row(&format!("SELECT CASE WHEN state IN {} THEN NULL ELSE member END FROM (SELECT member,state FROM effects WHERE team=?1 AND node=?2 AND generation=?3 ORDER BY seq DESC LIMIT 1)",crate::teams::store::NOT_RECEIVED),params![ctx.team.id,node,ctx.generation(node)?],|r|r.get::<_,Option<String>>(0)).optional().map(Option::flatten).map_err(storage)
}
/// The member a Return or Reassign reopens the node for: a Reassign's
/// assignee, a Return's holder.
pub(super) fn reopen_target(ctx: &Context<'_>, node: u32, kind: &str) -> Result<Option<String>> {
    if kind == "reassign" {
        Ok(ctx.node(node)?.assignee)
    } else {
        holder(ctx, node)
    }
}
pub(super) fn intent(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::HandoutIntent {
        node,
        member,
        effect_request_id,
    } = ctx.op
    else {
        unreachable!()
    };
    handout_admissible(ctx.db, ctx.team, *node, member)?;
    reserve(ctx, *node, member, effect_request_id, "handout")
}
/// The intent's node/member predicate, also read under the spawn admission.
/// An existing open effect is deliberately not part of this read.
pub(crate) fn handout_admissible(
    db: &Connection,
    team: &Team,
    node: u32,
    member: &str,
) -> Result<()> {
    let pending: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM decisions WHERE team=?1 AND node=?2 AND state<>'settled')",
            params![team.id, node],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if pending {
        return Err(invalid("The previous decision has not settled."));
    }
    let live: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM nodes WHERE team=?1 AND id=?2)",
            params![team.id, node],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if !live {
        return Err(invalid("Task has left the live plan."));
    }
    let n = rows::node(db, &team.id, node)?.ok_or_else(missing)?;
    require_ready(db, team, node)?;
    if n.state != NodeState::Pending || n.hold || n.assignee.as_ref().is_some_and(|id| id != member)
    {
        return Err(invalid("Task is not ready for a hand-out."));
    }
    eligible(db, team, &n, member)
}
fn eligible(db: &Connection, team: &Team, node: &Node, id: &str) -> Result<()> {
    if team.status != TeamStatus::Running {
        return Err(invalid("Team is not running."));
    }
    let m = rows::member(db, &team.id, id)?
        .ok_or_else(|| Problem::new(code::UNKNOWN_MEMBER, "Member does not exist."))?;
    // A stage never goes to a member who implemented any round of what it
    // checks: its own record, the node's current implementer, or the
    // target's recorded earlier implementers (zerus-63rru.20). The kernel's
    // staffing carries the same lists for the unstaffed reading. A record
    // read fails closed: an unreadable record refuses the hand-out.
    let earlier = if node.kind == NodeKind::Stage {
        node.implementer.as_deref() == Some(id)
            || match node.stage_of {
                Some(target) => rows::earlier_implementers(db, &team.id, target)
                    .map_err(storage)?
                    .iter()
                    .any(|m| m == id),
                None => false,
            }
    } else {
        false
    };
    if m.state != MemberState::Active
        || m.session.is_none()
        || m.id == team.lead
        || m.role != node.role
        || earlier
    {
        return Err(invalid("Member cannot take this task."));
    }
    Ok(())
}
fn reserve(ctx: &mut Context<'_>, node: u32, member: &str, id: &str, kind: &str) -> Result<()> {
    uuid(id)?;
    let used: bool = ctx
        .db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM effects WHERE request_id=?1)",
            [id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if used {
        return Err(invalid("Effect request ID was already used."));
    }
    let busy:bool=ctx.db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM effects WHERE team=?1 AND (node=?2 OR member=?3) AND state IN {OPEN_EFFECT_STATES})"),params![ctx.team.id,node,member],|r|r.get(0)).map_err(storage)?;
    if busy {
        return Err(invalid("Node or member has an unresolved effect."));
    }
    ctx.db.execute("INSERT INTO effects(team,node,member,generation,request_id,actor,kind,created_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![ctx.team.id,node,member,ctx.generation(node)?,id,encode(ctx.actor)?,kind,ctx.now]).map_err(storage)?;
    ctx.reply.node = Some(node);
    ctx.reply.item = Some(ctx.db.last_insert_rowid() as u64);
    ctx.db
        .execute(
            "INSERT INTO handout_bases(effect,base_commit) VALUES(?1,?2)",
            params![id, ctx.team.base_commit],
        )
        .map_err(storage)?;
    Ok(())
}
pub(super) fn reopen_intent(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::ReopenIntent {
        node,
        member,
        effect_request_id,
    } = ctx.op
    else {
        unreachable!()
    };
    let n = ctx.live_node(*node)?;
    if !in_hands(ctx, *node)? {
        return Err(invalid(
            "Reopen requires a task in somebody's hands; the hand-out follows instead.",
        ));
    }
    let (seq, kind, state) = ctx
        .pending(*node)?
        .ok_or_else(|| invalid("No reopen decision is pending."))?;
    if !matches!(kind.as_str(), "return" | "reassign") || state != DecisionState::Pending {
        return Err(invalid("Reopen was already reserved or is not requested."));
    }
    let target = reopen_target(ctx, *node, &kind)?;
    if target.as_deref() != Some(member) {
        return Err(invalid(
            "Reopen recipient differs from the decision's holder.",
        ));
    }
    eligible(ctx.db, ctx.team, &n, member)?;
    let busy:bool=ctx.db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM effects WHERE team=?1 AND member=?2 AND node<>?3 AND state IN {OPEN_EFFECT_STATES})"),params![ctx.team.id,member,node],|r|r.get(0)).map_err(storage)?;
    if busy {
        return Err(invalid("Reopen recipient is busy."));
    }
    ctx.db.execute(&format!("UPDATE effects SET state='superseded' WHERE team=?1 AND node=?2 AND state IN {OPEN_EFFECT_STATES}"),params![ctx.team.id,node]).map_err(storage)?;
    let mut revised = n;
    ctx.advance(*node, &mut revised)?;
    ctx.db
        .execute(
            "UPDATE decisions SET state=?3,generation=?2 WHERE seq=?1",
            params![seq, ctx.generation(*node)?, DecisionState::EffectReserved],
        )
        .map_err(storage)?;
    reserve(ctx, *node, member, effect_request_id, "reopen")
}
pub(super) fn outcome(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::HandoutOutcome {
        effect_request_id,
        outcome,
    } = ctx.op
    else {
        unreachable!()
    };
    record_outcome(ctx, effect_request_id, outcome, "handout")
}
pub(super) fn reopen_outcome(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::ReopenOutcome {
        effect_request_id,
        outcome,
    } = ctx.op
    else {
        unreachable!()
    };
    record_outcome(ctx, effect_request_id, outcome, "reopen")
}
fn record_outcome(
    ctx: &mut Context<'_>,
    id: &str,
    outcome: &HandoutOutcome,
    kind: &str,
) -> Result<()> {
    let e = read_effect(ctx, id)?;
    let same_non_delivery = matches!(outcome, HandoutOutcome::CuttingOver { .. })
        && e.outcome
            .as_deref()
            .and_then(|raw| serde_json::from_str::<HandoutOutcome>(raw).ok())
            .as_ref()
            == Some(outcome);
    let item = if same_non_delivery {
        store::non_delivery::non_delivery_item(
            ctx.db,
            &ctx.team.id,
            id,
            e.node,
            e.generation,
            kind,
            outcome,
        )?
    } else {
        None
    };
    let repairing = same_non_delivery
        && store::non_delivery::legacy_non_delivery_state(&e.state)
        && (e.state == "cutting_over" || item.is_some());
    if e.kind != kind || (e.outcome.is_some() && !repairing) {
        return Err(invalid(
            "Effect kind differs or its outcome was already recorded.",
        ));
    }
    let (state, why) = outcome_state(outcome)?;
    let current = e.generation == ctx.generation(e.node)?;
    let open: bool = ctx
        .db
        .query_row(
            &format!(
                "SELECT state IN {OPEN_EFFECT_STATES} FROM effects WHERE team=?1 AND request_id=?2"
            ),
            params![ctx.team.id, id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if !repairing {
        ctx.db
            .execute(
                "UPDATE effects SET outcome=?3 WHERE team=?1 AND request_id=?2",
                params![ctx.team.id, id, encode(outcome)?],
            )
            .map_err(storage)?;
        timers::outcome_time(ctx, id)?;
    }
    if let Some(item) = item {
        ctx.db.execute("UPDATE inbox SET state='closed' WHERE team=?1 AND seq=?2 AND kind='delivery_uncertain' AND state='open'",params![ctx.team.id,item]).map_err(storage)?;
    }
    // Closing proven non-delivery cannot resurrect a stale generation. Its
    // original outcome, receipt and timing remain facts, never a new offer.
    if repairing && e.state == "cutting_over" {
        ctx.db
            .execute(
                "UPDATE effects SET state='refused' WHERE team=?1 AND request_id=?2",
                params![ctx.team.id, id],
            )
            .map_err(storage)?;
    }
    // A late receipt is still a fact. It never resurrects a superseded,
    // reported or observed-done slot and never settles a newer decision.
    if current && e.state != "superseded" && state == "submitted" {
        record_implementer(ctx, e.node, &e.member)?;
    }
    if !current || !open {
        return Ok(());
    }
    ctx.db
        .execute(
            "UPDATE effects SET state=?3 WHERE team=?1 AND request_id=?2",
            params![ctx.team.id, id, state],
        )
        .map_err(storage)?;
    // A stopped or archived team records the outcome as a fact only. Its own
    // Stop settled every decision and wrote each node's Cancel, so moving a
    // decision here would settle that Cancel, re-hold the node or tell the
    // lead that the person already ended the work. The same gate covers the
    // informational delivery_uncertain item: a terminal team writes no lead
    // item at all (zerus-63rru.102 round 4).
    // A running or paused team keeps the move and the item it had before.
    let moving = matches!(ctx.team.status, TeamStatus::Running | TeamStatus::Paused);
    if let Some(why) = why {
        if moving {
            ctx.reply.item = Some(ctx.inbox(e.node, "delivery_uncertain", why)?);
        }
    }
    if kind == "handout" && moving {
        if let HandoutOutcome::HandedBack { reason } = outcome {
            let kind = format!("handout_handed_back:{id}");
            ctx.reply.item = Some(super::members::hand_to_lead(ctx, e.node, &kind, reason)?);
            return Ok(());
        }
    }
    if kind == "reopen" && moving && ctx.reopening(e.node)? {
        if let HandoutOutcome::HandedBack { reason } = outcome {
            // At once, never retried: the decision settled, the node held and
            // one inbox item for the lead (zerus-63rru.64).
            let kind = format!("reopen_handed_back:{id}");
            ctx.reply.item = Some(super::members::hand_to_lead(ctx, e.node, &kind, reason)?);
            return Ok(());
        }
        ctx.settle(e.node)?;
        // A reopen that was not delivered hands the node back to the lead;
        // a delivered one ends the decision's hold with the decision.
        if state != "submitted" {
            queue::hand_back(ctx, e.node, true)?;
        }
        if state == "submitted" {
            ctx.close_inbox(e.node)?;
        } else if let HandoutOutcome::Refused { reason } | HandoutOutcome::CuttingOver { reason } =
            outcome
        {
            // A held node with nobody told is the fault zerus-63rru.77
            // closes: an uncertain end already wrote its item above.
            let reason = reason.trim_end();
            let stop = if reason.ends_with(['.', '!', '?']) {
                ""
            } else {
                "."
            };
            let text = format!(
                "The reopen of task {} for member {} was not delivered: {reason}{stop} The decision is settled and the task waits for the lead.",
                e.node, e.member
            );
            ctx.reply.item = Some(ctx.inbox(e.node, &format!("reopen_refused:{id}"), &text)?);
        }
    }
    // A refused delivery may have been the only reason a Return or Reassign
    // waits for a hand-over; a proven non-start is recorded the same way.
    // Only while the team may still move a decision: a terminal team records
    // the fact alone (zerus-63rru.102 round 3).
    if state == "refused" && moving {
        settle_without_reason(ctx, e.node)?;
    }
    Ok(())
}
fn outcome_state(outcome: &HandoutOutcome) -> Result<(&'static str, Option<&str>)> {
    Ok(match outcome {
        HandoutOutcome::Submitted { receipt } => {
            reason(receipt)?;
            ("submitted", None)
        }
        HandoutOutcome::Refused { reason: r } | HandoutOutcome::HandedBack { reason: r } => {
            reason(r)?;
            ("refused", None)
        }
        HandoutOutcome::CuttingOver { reason: r } => {
            reason(r)?;
            ("refused", None)
        }
        HandoutOutcome::Uncertain { reason: r } => {
            reason(r)?;
            ("uncertain", Some(r.as_str()))
        }
    })
}
fn read_effect(ctx: &Context<'_>, id: &str) -> Result<Effect> {
    ctx.db.query_row("SELECT node,member,generation,kind,state,outcome FROM effects WHERE team=?1 AND request_id=?2",params![ctx.team.id,id],|r|Ok(Effect{node:r.get(0)?,member:r.get(1)?,generation:r.get(2)?,kind:r.get(3)?,state:r.get(4)?,outcome:r.get(5)?})).optional().map_err(storage)?.ok_or_else(missing)
}
/// Records `member` as an earlier implementer of the target `node`
/// (zerus-63rru.20). Called only by the service's own operations, in their
/// transaction; the record per target is bounded by the team's member
/// bound, so no separate cap is needed.
pub(super) fn record_earlier(ctx: &Context<'_>, node: u32, member: &str) -> Result<()> {
    ctx.db
        .execute(
            "INSERT OR IGNORE INTO target_implementers(team,target,member,round) VALUES(?1,?2,?3,?4)",
            params![ctx.team.id, node, member, ctx.generation(node)?],
        )
        .map_err(storage)?;
    Ok(())
}

fn record_implementer(ctx: &Context<'_>, node: u32, member: &str) -> Result<()> {
    let mut n = ctx.node(node)?;
    // A stage keeps the implementer copied from the task it checks. Its own
    // reviewer/tester lives only in the effect row and never replaces that ID.
    if n.kind == NodeKind::Task {
        if let Some(previous) = n.implementer.as_deref().filter(|p| *p != member) {
            record_earlier(ctx, node, previous)?;
        }
        n.implementer = Some(member.into());
        let table = ctx.node_table(node)?;
        ctx.db
            .execute(
                &format!("UPDATE {table} SET data=?3 WHERE team=?1 AND id=?2"),
                params![ctx.team.id, node, rows::node_payload(&n)?],
            )
            .map_err(storage)?;
    }
    Ok(())
}
/// The most service notes one report carries, the longest code and the
/// longest optional text. Codes are short lower-case tokens; the member's
/// report text is never checked or changed here.
const MAX_REPORT_NOTES: usize = 8;
const MAX_NOTE_CODE: usize = 32;
const MAX_NOTE_TEXT: usize = 500;

fn report_notes(notes: &[ReportNote]) -> Result<()> {
    if notes.len() > MAX_REPORT_NOTES {
        return Err(over("A report carries too many service notes."));
    }
    for note in notes {
        if !note.parameters.is_empty() {
            return Err(invalid(
                "Only the provider stop writer supplies report note parameters.",
            ));
        }
        let token = !note.code.is_empty()
            && note.code.len() <= MAX_NOTE_CODE
            && note.code.starts_with(|c: char| c.is_ascii_lowercase())
            && note
                .code
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !token {
            return Err(invalid(
                "A report note's code is not a short lower-case token.",
            ));
        }
        if let Some(text) = &note.text {
            if text.len() > MAX_NOTE_TEXT {
                return Err(over("A report note's text exceeds its size limit."));
            }
        }
    }
    Ok(())
}

pub(super) fn report(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::RecordReport {
        node,
        generation,
        member,
        text: body,
        commit,
        notes,
    } = ctx.op
    else {
        unreachable!()
    };
    ctx.active()?;
    let mut n = ctx.live_node(*node)?;
    if n.state != NodeState::Reported
        || *generation != ctx.generation(*node)?
        || recipient(ctx, *node)?.as_deref() != Some(member)
    {
        return Err(invalid(
            "Report does not belong to the current reviewed attempt's holder.",
        ));
    }
    let recorded:bool=ctx.db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM effects WHERE team=?1 AND node=?2 AND generation=?3 AND state NOT IN {})",crate::teams::store::NOT_RECEIVED),params![ctx.team.id,node,generation],|r|r.get(0)).map_err(storage)?;
    if !recorded {
        return Err(invalid("No current effect is available to report."));
    }
    let reported: bool = ctx
        .db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM reports WHERE team=?1 AND node=?2 AND generation=?3)",
            params![ctx.team.id, node, generation],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if reported {
        return Err(invalid("This attempt already has a report."));
    }
    reason(body)?;
    text(commit, 256)?;
    report_notes(notes)?;
    ctx.db.execute("INSERT INTO reports(team,node,generation,actor,text,commit_id,notes) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![ctx.team.id,node,generation,encode(ctx.actor)?,body,commit,encode(notes)?]).map_err(storage)?;
    // A trusted report proves delivery even when its submission outcome has
    // not arrived. Freeze that implementer before choosing any stage staff.
    if n.kind != NodeKind::Stage {
        if let Some(previous) = n.implementer.as_deref().filter(|p| *p != member) {
            record_earlier(ctx, *node, previous)?;
        }
        n.implementer = Some(member.clone());
        ctx.db
            .execute(
                "UPDATE nodes SET data=?3 WHERE team=?1 AND id=?2",
                params![ctx.team.id, node, rows::node_payload(&n)?],
            )
            .map_err(storage)?;
    }
    ctx.db.execute(&format!("UPDATE effects SET state='reported' WHERE team=?1 AND node=?2 AND generation=?3 AND state IN {OPEN_EFFECT_STATES}"),params![ctx.team.id,node,generation]).map_err(storage)?;
    if ctx.reopening(*node)? {
        ctx.settle(*node)?;
    }
    pipeline::reported(ctx, &n, body)
}
/// A report the removed member wrote after its removal took the task back
/// (zerus-63rru.77). The team does not record it: the lead is told once per
/// node and generation, with where the work is, and a second telling is a
/// duplicate. Only for the generation whose newest effect the removal
/// settled.
pub(super) fn late_report(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::RecordLateReport {
        node,
        generation,
        member,
    } = ctx.op
    else {
        unreachable!()
    };
    ctx.active()?;
    let n = ctx.live_node(*node)?;
    let m = ctx.member(member)?;
    let newest: Option<(String, String)> = ctx.db.query_row("SELECT member,state FROM effects INDEXED BY effects_attempt WHERE team=?1 AND node=?2 AND generation=?3 ORDER BY seq DESC LIMIT 1",params![ctx.team.id,node,generation],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(storage)?;
    let recorded: bool = ctx
        .db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM reports WHERE team=?1 AND node=?2 AND generation=?3)",
            params![ctx.team.id, node, generation],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if n.state != NodeState::Reported
        || *generation != ctx.generation(*node)?
        || m.state != MemberState::Removed
        || recorded
        || newest
            .as_ref()
            .map(|(who, state)| (who.as_str(), state.as_str()))
            != Some((member.as_str(), "superseded"))
    {
        return Err(invalid(
            "No report of a removed member waits here after its removal.",
        ));
    }
    let told: bool = ctx
        .db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM inbox WHERE team=?1 AND kind='removed_report' AND node=?2 AND generation=?3)",
            params![ctx.team.id, node, generation],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if told {
        return Err(Problem::new(
            code::DUPLICATE,
            "This late report was already told.",
        ));
    }
    let text = super::members::late_report_text(member, *node, &m.worktree);
    ctx.reply.node = Some(*node);
    ctx.reply.item = Some(ctx.inbox(*node, "removed_report", &text)?);
    Ok(())
}

fn require_ready(db: &Connection, team: &Team, node: u32) -> Result<()> {
    let Readiness { phases, nodes } = readiness::load(db, &team.id)?;
    let view = zerus_team_plan::ready::PlanView {
        phases: &phases,
        nodes: &nodes,
        members: &[],
        lead: &team.lead,
        integration: team.settings.integration,
    };
    // ready() does not choose members. The service owns the external native
    // free-member proof; eligible() and reserve() retain the local slot checks.
    if !zerus_team_plan::ready::ready(&view).contains(&node) {
        return Err(invalid(
            "The plan kernel does not consider this node ready.",
        ));
    }
    Ok(())
}
pub(super) fn kernel_nodes(ctx: &Context<'_>) -> Result<Vec<Node>> {
    readiness::nodes(ctx.db, &ctx.team.id)
}

/// Whether the task may be in somebody's hands, so that a Reassign or a
/// Return goes the hand-over route: the newest observation shows the task In
/// progress, In review or Done (not cancelled, whoever executes it, a
/// recorded conflict of attempts included), or an effect of the node's
/// current generation is still open. Every open state counts: a reservation
/// is no proof that its child never started, and an observation lags the
/// send it follows. Otherwise the task is To do, Backlog or unobserved and
/// nothing of its generation is open, and the ordinary hand-out follows the
/// node's assignee. The attempt the node remembers is not read: it is never
/// cleared, and a task is To do again only after its run reported or was
/// done, or after a send that never started.
pub(super) fn in_hands(ctx: &Context<'_>, node: u32) -> Result<bool> {
    ctx.db
        .query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM task_observations WHERE team=?1 AND node=?2 AND NOT json_extract(data,'$.cancelled') AND (json_extract(data,'$.stage') IN ('in_progress','in_review','done') OR attempt_conflict)) OR EXISTS(SELECT 1 FROM effects WHERE team=?1 AND node=?2 AND generation=?3 AND state IN {OPEN_EFFECT_STATES})"),
            params![ctx.team.id, node, ctx.generation(node)?],
            |r| r.get(0),
        )
        .map_err(storage)
}
/// A Pending Return or Reassign does not outlive its reason: otherwise no
/// hand-over would carry it out, and it would refuse every other decision of
/// its node and block the ordinary hand-out. In the operation that took the
/// reason away:
/// - a Return whose holder reads nobody is handed back to the lead, as a
///   refused reopen is (settled, the node held, one item for the lead);
/// - a Return or a Reassign whose task is in nobody's hands any more is
///   settled, and the decision's hold ends with it; the ordinary hand-out
///   then follows the node's assignee unless an explicit hold remains, which
///   only its owner's Release ends.
///
/// A decision whose reopen is reserved has started and waits for its
/// outcome.
pub(super) fn settle_without_reason(ctx: &mut Context<'_>, node: u32) -> Result<()> {
    let Some((seq, kind, DecisionState::Pending)) = ctx.pending(node)? else {
        return Ok(());
    };
    match kind.as_str() {
        "return" if holder(ctx, node)?.is_none() => {
            let actor = encode(ctx.actor)?;
            ctx.reply.item = Some(crate::teams::store::hand_back_return(
                ctx.db,
                &ctx.team.id,
                node,
                seq,
                &actor,
                ctx.now,
            )?);
        }
        // The decision's hold ends with it; an explicit hold stays.
        "return" | "reassign" if !in_hands(ctx, node)? => ctx.settle(node)?,
        _ => (),
    }
    Ok(())
}
