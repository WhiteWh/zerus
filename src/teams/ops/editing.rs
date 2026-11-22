//! Team edits preserve role defaults and existing seat identities.
use super::*;
use zerus_team_plan::{catalog::AccountChoice, member::MemberLaunch};

pub(super) fn rename(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::RenameTeam { name } = ctx.op else {
        unreachable!()
    };
    ctx.active()?;
    if name.trim().is_empty() {
        return Err(invalid("A team name is required."));
    }
    if let Some(problem) = zerus_team_plan::name_problem(name, "The team name", 256) {
        return Err(problem);
    }
    if *name == ctx.team.name {
        return Err(invalid("The team name is unchanged."));
    }
    ctx.db
        .execute(
            "UPDATE teams SET name=?2 WHERE id=?1",
            params![ctx.team.id, name],
        )
        .map_err(storage)?;
    Ok(())
}
pub(super) fn goal(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::SetGoal { goal } = ctx.op else {
        unreachable!()
    };
    ctx.active()?;
    text(goal, 16_384)?;
    if *goal == ctx.team.goal {
        return Err(invalid("The team goal is unchanged."));
    }
    ctx.db
        .execute(
            "UPDATE teams SET goal=?2 WHERE id=?1",
            params![ctx.team.id, goal],
        )
        .map_err(storage)?;
    Ok(())
}
fn choices(agent: &str, model: &str, effort: &str, account: &AccountChoice) -> Result<()> {
    crate::teams::catalog::validate_member_agent(agent)?;
    if agent == "dsh" && !model.is_empty() {
        return Err(invalid("DeepSeek model is managed in its native account."));
    }
    if !matches!(agent, "claude" | "codex") && !effort.is_empty() {
        return Err(invalid(
            "This agent has no supported launch effort argument.",
        ));
    }
    for (field, value, limit) in [
        ("The member model", model, 256),
        ("The member effort", effort, 64),
    ] {
        if let Some(problem) = zerus_team_plan::name_problem(value, field, limit) {
            return Err(problem);
        }
    }
    if let AccountChoice::Named { account } = account {
        if account.trim().is_empty() {
            return Err(invalid("A named account needs its ID."));
        }
        if let Some(problem) = zerus_team_plan::name_problem(account, "The member account", 256) {
            return Err(problem);
        }
    }
    Ok(())
}
pub(super) fn add_member(ctx: &mut Context<'_>) -> Result<()> {
    let (Operation::AddMember {
        role,
        agent,
        model,
        effort,
        account,
    }
    | Operation::LeadAddMember {
        role,
        agent,
        model,
        effort,
        account,
    }) = ctx.op
    else {
        unreachable!()
    };
    ctx.active()?;
    if let Some(p) = zerus_team_plan::validate::id_problem(role, "member.role") {
        return Err(p);
    }
    if !ctx.team.bundle.roles.iter().any(|r| r.id == *role) {
        return Err(Problem::new(
            code::UNKNOWN_ROLE,
            "Choose a role in this team's pinned catalog.",
        ));
    }
    choices(agent, model, effort, account)?;
    let ids = ctx
        .db
        .prepare("SELECT id,state FROM members WHERE team=?1 ORDER BY id LIMIT 202")
        .and_then(|mut s| {
            s.query_map([&ctx.team.id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(storage)?;
    // The existing engine bounds the whole retained roster at 201, including
    // the lead. Removed IDs are never recycled into somebody else's identity.
    if ids.len() >= 201 {
        return Err(over("This team retains the maximum 201 member records."));
    }
    let active = ids
        .iter()
        .filter(|(id, state)| id != &ctx.team.lead && state != "\"removed\"")
        .count();
    if active >= ctx.team.settings.caps.max_members as usize {
        return Err(over("The team has its maximum number of members."));
    }
    let next = ids
        .iter()
        .filter_map(|(id, _)| id.strip_prefix('m')?.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| over("Member IDs are exhausted."))?;
    let member = Member {
        id: format!("m{next}"),
        role: role.clone(),
        agent: agent.clone(),
        launch: Some(MemberLaunch {
            generation: 0,
            model: model.clone(),
            effort: effort.clone(),
            account: account.clone(),
        }),
        account: String::new(),
        state: MemberState::NotCreated,
        session: None,
        worktree: String::new(),
        ownership_nonce: String::new(),
    };
    ctx.db
        .execute(
            "INSERT INTO members(team,id,data,state) VALUES(?1,?2,?3,?4)",
            params![
                ctx.team.id,
                member.id,
                encode(&member)?,
                encode(&member.state)?
            ],
        )
        .map_err(storage)?;
    if matches!(ctx.op, Operation::LeadAddMember { .. }) {
        ctx.reply.item = Some(staffing_notice(ctx, &member)?);
    }
    ctx.reply.member = Some(member.id);
    Ok(())
}

fn staffing_notice(ctx: &Context<'_>, member: &Member) -> Result<u64> {
    let id = &member.id;
    let role = &member.role;
    let agent = match member.agent.as_str() {
        "claude" => "Claude",
        "codex" => "Codex",
        "kimi" => "Kimi",
        "dsh" => "DeepSeek",
        agent => agent,
    };
    let kind = format!("lead_member_added:{id}");
    let text = format!("The lead added member {id} ({agent}) to staff the {role} role.");
    let notes = vec![ReportNote {
        code: "lead_member_added".into(),
        text: Some(text.clone()),
        parameters: vec![id.clone(), agent.into(), role.clone()],
    }];
    ctx.db.execute(
        "INSERT INTO inbox(team,kind,node,generation,actor,text,notes,state,created_ms) VALUES(?1,?2,0,0,?3,?4,?5,'closed',?6)",
        params![ctx.team.id, kind, encode(ctx.actor)?, text, encode(&notes)?, ctx.now],
    ).map_err(storage)?;
    Ok(ctx.db.last_insert_rowid() as u64)
}

/// Swap changes only the seat's next launch. No native stop, archive or send
/// is performed by this operation; the old exact binding loses team authority.
pub(super) fn swap_member(ctx: &mut Context<'_>) -> Result<()> {
    let Operation::SwapMember {
        member,
        agent,
        model,
        effort,
        account,
    } = ctx.op
    else {
        unreachable!()
    };
    ctx.active()?;
    choices(agent, model, effort, account)?;
    let mut m = ctx.member(member)?;
    let work = store::editing::member_work(ctx.db, &ctx.team.id)?
        .get(member)
        .copied()
        .unwrap_or(0);
    if m.state != MemberState::Paused || !ctx.person_stopped(member)? || work != 0 {
        return Err(invalid(&format!("Stop the member first; open work: {work}. Swap requires a person-stopped member with no open or unsettled work.")));
    }
    crate::desktop::teams::member_swap::check(ctx.state, ctx.team, &m)?;
    let generation = m
        .launch
        .as_ref()
        .map_or(0, |l| l.generation)
        .checked_add(1)
        .ok_or_else(|| over("Member launch generations are exhausted."))?;
    m.launch = Some(MemberLaunch {
        generation,
        model: model.clone(),
        effort: effort.clone(),
        account: account.clone(),
    });
    m.agent = agent.clone();
    m.account.clear();
    m.session = None;
    m.state = MemberState::NotCreated;
    members::save(ctx, &m)?;
    ctx.db
        .execute(
            "UPDATE members SET person_stopped=0 WHERE team=?1 AND id=?2",
            params![ctx.team.id, member],
        )
        .map_err(storage)?;
    ctx.db
        .execute(
            "DELETE FROM member_handles WHERE team=?1 AND member=?2",
            params![ctx.team.id, member],
        )
        .map_err(storage)?;
    ctx.reply.member = Some(member.clone());
    Ok(())
}
