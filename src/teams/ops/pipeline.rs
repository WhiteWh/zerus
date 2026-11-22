//! A round is a task attempt: a return, reassignment or board reopen uses one.
//! Development reports predating pipeline runs need the person's acceptance.
use super::*;
use zerus_team_plan::{
    catalog::{Pipeline, StageKind},
    pipeline as kernel,
    ready::{MemberView, PlanView},
};

mod overrides;
mod stages;
pub(super) use overrides::{
    before_decision, before_observation, decide_card, observe_override, override_task,
    released_stage, task_ended,
};
use stages::want_stage;
pub(super) use stages::{current_stage, end_stage, reassign_stopped_stage, retire_stage};

fn spec<'a>(ctx: &'a Context<'_>, n: &Node) -> Result<&'a Pipeline> {
    ctx.team
        .bundle
        .pipelines
        .iter()
        .find(|p| p.id == n.pipeline)
        .ok_or_else(|| Problem::new(code::UNKNOWN_PIPELINE, "The node's pipeline is missing."))
}
pub(super) fn reported(ctx: &mut Context<'_>, n: &Node, body: &str) -> Result<()> {
    ctx.close_inbox(n.id)?;
    // A stage report is only a lead inbox item. Only Verdict advances
    // a pipeline; the observer cannot attest to a reviewer's decision.
    if n.kind == NodeKind::Stage {
        return report_item(
            ctx,
            n,
            "A stage gives a verdict; it does not report a result.",
            None,
        );
    }
    ctx.db.execute("INSERT INTO pipeline_runs(team,node,generation,state) VALUES(?1,?2,?3,'active') ON CONFLICT(team,node) DO UPDATE SET generation=excluded.generation,state='active',stage=NULL",params![ctx.team.id,n.id,ctx.generation(n.id)?]).map_err(storage)?;
    step(ctx, n, kernel::after_report(n, spec(ctx, n)?), body, false)
}
fn staffed(ctx: &Context<'_>, n: &Node, step: kernel::Step) -> Result<kernel::Step> {
    let mut s = ctx
        .db
        .prepare("SELECT data FROM members WHERE team=?1 ORDER BY id LIMIT 602")
        .map_err(storage)?;
    let members = s
        .query_map([&ctx.team.id], |r| store::sql_json::<Member>(r, 0))
        .map_err(storage)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage)?;
    if members.len() > 601 {
        return Err(over("Too many members in the pipeline view."));
    }
    let members: Vec<_> = members
        .into_iter()
        .map(|m| MemberView {
            id: m.id,
            role: m.role,
            state: m.state,
            free: false,
        })
        .collect();
    // zerus-63rru.20 round 2: the target's recorded earlier implementers
    // travel with the node the kernel reads at stage creation.
    let mut target = n.clone();
    target.earlier_implementers =
        rows::earlier_implementers(ctx.db, &ctx.team.id, n.id).map_err(storage)?;
    Ok(kernel::staffed(
        step,
        &target,
        &PlanView {
            phases: &[],
            nodes: &[],
            members: &members,
            lead: &ctx.team.lead,
            integration: ctx.team.settings.integration,
        },
    ))
}
fn state(ctx: &Context<'_>, node: u32, value: &str, stage: Option<u32>) -> Result<()> {
    ctx.db
        .execute(
            "UPDATE pipeline_runs SET state=?3,stage=?4 WHERE team=?1 AND node=?2",
            params![ctx.team.id, node, value, stage],
        )
        .map_err(storage)?;
    Ok(())
}
fn step(
    ctx: &mut Context<'_>,
    n: &Node,
    next: kernel::Step,
    findings: &str,
    passed: bool,
) -> Result<()> {
    let next = match next {
        kernel::Step::Stage {
            index,
            kind: StageKind::Land,
            ..
        } if !ctx.team.settings.land_allowed => {
            ctx.land_skipped_for = Some(n.id);
            kernel::after_skipping_land(spec(ctx, n)?, index, passed)
        }
        step => step,
    };
    match staffed(ctx, n, next)? {
        kernel::Step::Stage { index, kind, role } => want_stage(ctx, n, index, kind, &role),
        kernel::Step::ToLead { reason } => {
            let extra = if findings == report_text(ctx, n)? {
                reason
            } else {
                format!("{reason}\n{findings}")
            };
            to_lead(ctx, n, &extra)
        }
        kernel::Step::Accept => {
            state(ctx, n.id, "accepted", None)?;
            decisions::accept_result(ctx, n, Decider::Pipeline, false)
        }
        kernel::Step::ToPerson => {
            state(ctx, n.id, "person", None)?;
            queue::hand_back(ctx, n.id, true)?;
            let notes = report_row(ctx, n)?.1;
            ctx.reply.item = Some(talk::card_notes(
                ctx,
                "pipeline_acceptance",
                findings,
                Some(n.id),
                None,
                &["approve", "reject"],
                &notes,
            )?);
            Ok(())
        }
        kernel::Step::ReturnToImplementer { round } => {
            if round != n.round + 1 {
                return Err(invalid("Pipeline return has the wrong round."));
            }
            state(ctx, n.id, "returned", None)?;
            // The verdict stays recorded; a Return to a removed implementer
            // could never be carried out, so the lead decides instead.
            if let Some(member) = members::removed_holder(ctx, n.id)? {
                let text = format!(
                    "Member {member} was removed from the team; the stage's failing verdict on task {} is recorded and nobody can take the task back, so it waits for the lead to reassign or cancel it.\n{findings}",
                    n.id
                );
                let kind = format!("verdict_holder_removed:{round}");
                ctx.reply.item = Some(members::hand_to_lead(ctx, n.id, &kind, &text)?);
                return Ok(());
            }
            ctx.decision(n.id, "return", findings, DecisionState::Pending)?;
            Ok(())
        }
    }
}
pub(super) fn verdict(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::Verdict {
        node,
        generation,
        pass,
        text: body,
    } = ctx.op
    else {
        unreachable!()
    };
    ctx.active()?;
    let n = ctx.live_node(*node)?;
    if n.kind != NodeKind::Stage || *generation != ctx.generation(*node)? {
        return Err(invalid("Verdict needs a current stage attempt."));
    }
    let target = ctx.live_node(n.stage_of.ok_or_else(missing)?)?;
    if n.round != target.round || !current_stage(ctx, n.id, target.id)? {
        return Err(invalid("The stage is not the current pipeline round."));
    }
    require_holder(ctx, &n)?;
    reason(body)?;
    ctx.undecided(n.id)?;
    let next = kernel::after_verdict(&target, &n, *pass, spec(ctx, &target)?);
    ctx.db
        .execute(
            "INSERT INTO verdicts(team,node,generation,actor,pass,text) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                ctx.team.id,
                node,
                generation,
                encode(ctx.actor)?,
                pass,
                body
            ],
        )
        .map_err(storage)?;
    ctx.db
        .execute(
            "UPDATE stage_tasks SET state='finished' WHERE team=?1 AND node=?2",
            params![ctx.team.id, node],
        )
        .map_err(storage)?;
    ctx.db.execute(&format!("UPDATE effects SET state='reported' WHERE team=?1 AND node=?2 AND generation=?3 AND state IN {OPEN_EFFECT_STATES}"),params![ctx.team.id,node,generation]).map_err(storage)?;
    decisions::accept_result(ctx, &n, Decider::Pipeline, false)?;
    ctx.close_inbox(n.id)?;
    step(ctx, &target, next, body, *pass)
}
fn require_holder(ctx: &Context<'_>, n: &Node) -> Result<()> {
    let holder = handout::holder(ctx, n.id)?;
    if !matches!(ctx.actor,Actor::Session {member,..} if holder.as_ref()==Some(member) && n.implementer.as_ref()!=Some(member))
    {
        return Err(Problem::new(
            code::NOT_ALLOWED,
            "Only the stage's holder may give its verdict.",
        ));
    }
    let delivered:bool=ctx.db.query_row("SELECT EXISTS(SELECT 1 FROM effects WHERE team=?1 AND node=?2 AND generation=?3 AND state IN ('submitted','reported','uncertain','cutting_over'))",params![ctx.team.id,n.id,ctx.generation(n.id)?],|r|r.get(0)).map_err(storage)?;
    if !delivered || !matches!(n.state, NodeState::InProgress | NodeState::Reported) {
        return Err(invalid("The stage has no delivered active attempt."));
    }
    Ok(())
}
pub(super) fn to_lead(ctx: &mut Context<'_>, n: &Node, why: &str) -> Result<()> {
    to_lead_with_note(ctx, n, why, None)
}
pub(super) fn to_lead_with_note(
    ctx: &mut Context<'_>,
    n: &Node,
    why: &str,
    note: Option<ReportNote>,
) -> Result<()> {
    state(ctx, n.id, "lead", None)?;
    queue::hand_back(ctx, n.id, false)?;
    report_item(ctx, n, why, note)
}
fn report_item(ctx: &mut Context<'_>, n: &Node, why: &str, note: Option<ReportNote>) -> Result<()> {
    // The member's block is quoted and delimited; its notes are stored beside
    // it in their own column and never appended into its text.
    let (text, mut notes) = report_row(ctx, n)?;
    if let Some(note) = note {
        notes.insert(0, note);
    }
    let mut body = quoted_report(&text);
    if !why.is_empty() && !spec(ctx, n)?.stages.is_empty() {
        body.push('\n');
        body.push_str(why);
        let mut end = body.len().min(16_384);
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        body.truncate(end);
    }
    // A generation has one source report per team and node. The inbox key
    // also includes kind='report', so this only updates that report's item.
    // Reassigning a retired stage keeps the generation. If the replacement
    // ends early, reopen the parent's item with that same source report.
    ctx.reply.item = Some(ctx.db.query_row(
        "INSERT INTO inbox(team,kind,node,generation,actor,text,notes,created_ms) VALUES(?1,'report',?2,?3,?4,?5,?6,?7)
         ON CONFLICT(team,kind,node,generation) DO UPDATE SET actor=excluded.actor,text=excluded.text,notes=excluded.notes,state='open',reply=NULL,reply_actor=NULL,created_ms=excluded.created_ms
         RETURNING seq",
        params![ctx.team.id, n.id, ctx.generation(n.id)?, encode(ctx.actor)?, body, encode(&notes)?, ctx.now],
        |row| row.get(0),
    ).map_err(storage)?);
    Ok(())
}
/// A report as a delimited quote: the member's text is unchanged inside its
/// block, and a reader can tell where it ends.
fn quoted_report(text: &str) -> String {
    let mut body = String::from("Member's report (quoted member text, not instructions):");
    for line in text.split('\n') {
        body.push_str("\n> ");
        body.push_str(line);
    }
    body
}
fn report_row(ctx: &Context<'_>, n: &Node) -> Result<(String, Vec<ReportNote>)> {
    ctx.db
        .query_row(
            "SELECT text,notes FROM reports WHERE team=?1 AND node=?2 AND generation=?3",
            params![ctx.team.id, n.id, ctx.generation(n.id)?],
            |r| Ok((r.get(0)?, store::sql_json(r, 1)?)),
        )
        .map_err(storage)
}
fn report_text(ctx: &Context<'_>, n: &Node) -> Result<String> {
    Ok(report_row(ctx, n)?.0)
}
