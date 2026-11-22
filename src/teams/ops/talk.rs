use super::*;

pub(super) fn ask(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::Ask {
        node,
        generation,
        text: body,
    } = ctx.op
    else {
        unreachable!()
    };
    ctx.active()?;
    let n = ctx.live_node(*node)?;
    let holder = handout::recipient(ctx, *node)?;
    if let Actor::Session { member, .. } = ctx.actor {
        if holder.as_ref() != Some(member) {
            return Err(Problem::new(
                code::NOT_ALLOWED,
                "Only the attempt holder may ask about it.",
            ));
        }
    }
    if *generation != ctx.generation(*node)?
        || !matches!(n.state, NodeState::InProgress | NodeState::Reported)
    {
        return Err(invalid("Question needs a current active attempt."));
    }
    reason(body)?;
    let count:u32=ctx.db.query_row("SELECT count(*) FROM inbox WHERE team=?1 AND node=?2 AND generation=?3 AND kind LIKE 'question:%' AND state='open'",params![ctx.team.id,node,generation],|r|r.get(0)).map_err(storage)?;
    if count >= ctx.team.settings.caps.max_open_questions {
        return Err(over("Open question cap reached."));
    }
    let next: u64 = ctx
        .db
        .query_row("SELECT COALESCE(MAX(seq),0)+1 FROM inbox", [], |r| r.get(0))
        .map_err(storage)?;
    ctx.reply.item = Some(ctx.inbox(*node, &format!("question:{next}"), body)?);
    Ok(())
}
pub(super) fn reply(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::Reply { item, text: body } = ctx.op else {
        unreachable!()
    };
    ctx.active()?;
    reason(body)?;
    let changed=ctx.db.execute("UPDATE inbox SET reply=?3,reply_actor=?4,state='closed' WHERE team=?1 AND seq=?2 AND state='open' AND kind LIKE 'question:%'",params![ctx.team.id,item,body,encode(ctx.actor)?]).map_err(storage)?;
    if changed != 1 {
        return Err(invalid("Question is missing or already answered."));
    }
    timers::mark_answer(ctx, *item)
}

pub(super) fn close_stale_reopen_notices(ctx: &mut Context<'_>) -> Result<()> {
    if store::non_delivery::close_stale_reopen_notices(ctx.db, &ctx.team.id)? == 0 {
        return Err(invalid("No stale reopen notices to close."));
    }
    Ok(())
}

pub(super) fn open_card(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::OpenCard {
        kind,
        text,
        node,
        decisions,
        parameters,
    } = ctx.op
    else {
        unreachable!()
    };
    planning::ordinary_card(ctx, *node)?;
    if matches!(kind.as_str(), "integration" | "pipeline_acceptance") {
        return Err(invalid(
            "Acceptance cards are opened by their pipeline or decision.",
        ));
    }
    if let Some(node) = node {
        ctx.node(*node)?;
    }
    let listed: Vec<&str> = match decisions {
        Some(list) => {
            listed(list)?;
            list.iter().map(String::as_str).collect()
        }
        None => Vec::new(),
    };
    let parameters = parameters.clone().unwrap_or_default();
    parameters_checked(&parameters)?;
    ctx.reply.item = Some(card(ctx, kind, text, *node, None, &listed, &parameters)?);
    Ok(())
}
/// The bounded parameters a localized window substitutes (zerus-63rru.34):
/// at most 8 pairs, each key lower-case ASCII letters and underscores, each
/// value nonempty, at most 256 characters, and free of control characters,
/// line or paragraph separators and bidirectional marks. An invalid value is
/// refused, never stripped. Written only by the service's own card writer.
pub(super) fn parameters_checked(parameters: &[(String, String)]) -> Result<()> {
    if parameters.len() > 8 {
        return Err(invalid("A card takes at most 8 parameters."));
    }
    for (key, value) in parameters {
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'_')
        {
            return Err(invalid("A card parameter key must be [a-z_]."));
        }
        reason(value)?;
        if value.chars().count() > 256 {
            return Err(invalid(
                "A card parameter value is longer than 256 characters.",
            ));
        }
        if crate::teams::clean::forbidden(value) {
            return Err(invalid(
                "A card parameter value holds a control character, a line separator or a bidirectional mark.",
            ));
        }
    }
    Ok(())
}

/// The decisions an opener lists: one to four distinct tokens of at most 32
/// bytes, lower-case ASCII letters and underscores, starting with a letter.
/// The internal stale mark is never one of them.
fn listed(list: &[String]) -> Result<()> {
    let token = |d: &String| {
        (1..=32).contains(&d.len())
            && d.starts_with(|c: char| c.is_ascii_lowercase())
            && d.chars().all(|c| c.is_ascii_lowercase() || c == '_')
            && d != "stale"
    };
    let distinct = list.iter().enumerate().all(|(i, d)| !list[..i].contains(d));
    if list.is_empty() || list.len() > 4 || !distinct || !list.iter().all(token) {
        return Err(invalid(
            "A card lists one to four distinct decisions, each a short lower-case word with underscores.",
        ));
    }
    Ok(())
}
/// Open a card. `decisions` are the ones something acts on; an empty list
/// stores none, and the card answers by the rule of its kind.
pub(super) fn card(
    ctx: &Context<'_>,
    kind: &str,
    body: &str,
    node: Option<u32>,
    decision: Option<u64>,
    decisions: &[&str],
    parameters: &[(String, String)],
) -> Result<u64> {
    write_card(ctx, kind, body, node, decision, decisions, (&[], parameters))
}

/// An open card that quotes a report: the service's notes are their own
/// column beside the text, never appended into it.
pub(super) fn card_notes(
    ctx: &Context<'_>,
    kind: &str,
    body: &str,
    node: Option<u32>,
    decision: Option<u64>,
    decisions: &[&str],
    notes: &[ReportNote],
) -> Result<u64> {
    write_card(ctx, kind, body, node, decision, decisions, (notes, &[]))
}

fn write_card(
    ctx: &Context<'_>,
    kind: &str,
    body: &str,
    node: Option<u32>,
    decision: Option<u64>,
    decisions: &[&str],
    (notes, parameters): (&[ReportNote], &[(String, String)]),
) -> Result<u64> {
    reason(kind)?;
    text(kind, 128)?;
    reason(body)?;
    let listed = if decisions.is_empty() {
        None
    } else {
        Some(encode(&decisions)?)
    };
    let parameters = if parameters.is_empty() {
        None
    } else {
        Some(encode(&parameters)?)
    };
    ctx.db.execute("INSERT INTO cards(team,kind,node,actor,text,notes,created_ms,decision_seq,decisions,parameters) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![ctx.team.id,kind,node,encode(ctx.actor)?,body,encode(&notes.to_vec())?,ctx.now,decision,listed,parameters]).map_err(storage)?;
    Ok(ctx.db.last_insert_rowid() as u64)
}
/// An open card's kind, node, decision sequence and stored decisions.
type OpenCardRow = (String, Option<u32>, Option<u64>, Option<String>);
pub(super) fn decide_card(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::DecideCard { card, decision } = ctx.op else {
        unreachable!()
    };
    reason(decision)?;
    let row: Option<OpenCardRow> = ctx
        .db
        .query_row(
            "SELECT kind,node,decision_seq,decisions FROM cards WHERE team=?1 AND seq=?2 AND state='open'",
            params![ctx.team.id, card],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(storage)?;
    let Some((kind, node, seq, listed)) = row else {
        return Err(unavailable_card(ctx, *card)?);
    };
    // A card takes only the decisions it lists, with the code of a wrong
    // integration answer; nothing is written.
    let takes = rows::card_decisions(&kind, listed.as_deref())?;
    if !takes.iter().any(|d| d == decision) {
        return Err(invalid(&format!(
            "This card takes {}, not {decision}.",
            takes.join(" or ")
        )));
    }
    let stale = if kind == "integration" {
        decide_integration(
            ctx,
            node.ok_or_else(missing)?,
            seq.ok_or_else(missing)?,
            decision,
        )?
    } else if kind == "pipeline_acceptance" {
        pipeline::decide_card(ctx, node.ok_or_else(missing)?, decision)?
    } else {
        planning::ordinary_card(ctx, node)?;
        false
    };
    if stale {
        ctx.event = Some("stale_card_decision");
    }
    ctx.db
        .execute(
            "UPDATE cards SET decision=?3,decided_actor=?4,state='closed' WHERE team=?1 AND seq=?2",
            params![
                ctx.team.id,
                card,
                if stale { "stale" } else { decision },
                encode(ctx.actor)?
            ],
        )
        .map_err(storage)?;
    // A merge recorded before this rejection is still in the integration
    // branch: the person is told once, without changing the decision.
    if kind == "integration" && decision == "reject" && !stale {
        let node = node.ok_or_else(missing)?;
        let seq = seq.ok_or_else(missing)?;
        if rows::merged_commit(ctx.db, &ctx.team.id, seq)?.is_some() {
            rows::rejected_merge(
                ctx.db,
                &ctx.team.id,
                node,
                seq,
                &ctx.team.integration_branch,
                &encode(ctx.actor)?,
                ctx.now,
            )?;
        }
    }
    Ok(())
}
fn decide_integration(ctx: &Context<'_>, node: u32, seq: u64, answer: &str) -> Result<bool> {
    if ctx.pending(node)? != Some((seq, "accept".into(), DecisionState::AwaitingOwner)) {
        return Ok(true);
    }
    ctx.active()?;
    match answer {
        "approve" => {
            ctx.db
                .execute(
                    "UPDATE decisions SET state=?2 WHERE seq=?1",
                    params![seq, DecisionState::Ready],
                )
                .map_err(storage)?;
        }
        "reject" => {
            ctx.settle(node)?;
            ctx.db
                .execute(
                    "UPDATE nodes SET decided_by=NULL WHERE team=?1 AND id=?2",
                    params![ctx.team.id, node],
                )
                .map_err(storage)?;
        }
        _ => return Err(invalid("Integration decision must be approve or reject.")),
    }
    Ok(false)
}

pub(super) fn close_card(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::CloseCard { card, reason: why } = ctx.op else {
        unreachable!()
    };
    planning::ordinary_card(ctx, None)?;
    reason(why)?;
    if !close_owned_card(ctx, *card, None)? {
        return Err(invalid(
            "Only an open ordinary card opened by this service can be closed.",
        ));
    }
    Ok(())
}

pub(super) fn close_owned_card(ctx: &Context<'_>, card: u64, kind: Option<&str>) -> Result<bool> {
    let changed = ctx.db.execute(
        "UPDATE cards SET state='closed',decision='stale',decided_actor=?3
         WHERE team=?1 AND seq=?2 AND state='open' AND kind NOT IN ('integration','pipeline_acceptance')
         AND actor=?3 AND (?4 IS NULL OR kind=?4)",
        params![ctx.team.id, card, encode(ctx.actor)?, kind],
    ).map_err(storage)?;
    Ok(changed == 1)
}

fn unavailable_card(ctx: &Context<'_>, card: u64) -> Result<Problem> {
    let stale = rows::card(ctx.db, &ctx.team.id, card)?
        .is_some_and(|c| c.decision.as_deref() == Some("stale"));
    Ok(invalid(if stale {
        "Card is stale."
    } else {
        "Card is missing or already decided."
    }))
}
