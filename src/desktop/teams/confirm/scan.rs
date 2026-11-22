//! One bounded roster page on the existing service worker, with progress kept
//! across passes. Large rosters cannot starve later members or later teams.
use super::{
    engine,
    native::Native,
    policy,
    ports::Result,
    types::{Binding, Facts},
};
use crate::desktop::teams::actor;
use crate::{
    config::Config,
    teams::store::{Cursor, Store},
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use zerus_team_plan::model::{Member, MemberState, Team, TeamStatus};
#[derive(Default)]
pub(in crate::desktop::teams) struct Scan {
    teams: Option<Cursor>,
    team: Option<String>,
    members: Option<Cursor>,
}
pub(in crate::desktop::teams) fn step(
    config: &Config,
    epoch: &str,
    machine: &str,
    store: &mut Store,
    stop: &Arc<AtomicBool>,
    cursor: &mut Scan,
    cards: &mut actor::Runtime,
) -> Result<()> {
    step_at(
        config,
        (epoch, machine),
        store,
        stop,
        (cursor, cards),
        || chrono::Utc::now().timestamp_millis(),
        |error| eprintln!("hgs: Teams standing confirmation fault: {error}"),
    )
}

fn step_at(
    config: &Config,
    service: (&str, &str),
    store: &mut Store,
    stop: &Arc<AtomicBool>,
    scan: (&mut Scan, &mut actor::Runtime),
    now: impl Fn() -> i64,
    mut report: impl FnMut(&str),
) -> Result<()> {
    let (epoch, machine) = service;
    let (cursor, cards) = scan;
    if stop.load(Ordering::Acquire) || !crate::teams::enabled(&config.state) {
        return Ok(());
    }
    store.refresh().map_err(|e| e.to_string())?;
    if cursor.team.is_none() {
        let page = store
            .live_teams(1, cursor.teams.as_ref())
            .map_err(|e| e.to_string())?;
        cursor.teams = page.next;
        cursor.team = page.rows.first().map(|team| team.id.clone());
    }
    let Some(team_id) = &cursor.team else {
        return Ok(());
    };
    let Some(team) = store.team(team_id).map_err(|e| e.to_string())? else {
        cursor.team = None;
        cursor.members = None;
        return Ok(());
    };
    let planning = team.status == TeamStatus::Planning
        && store
            .planning(team_id)
            .map_err(|e| e.to_string())?
            .is_some_and(|p| !p.paused);
    let page = store
        .members(team_id, 16, cursor.members.as_ref())
        .map_err(|e| e.to_string())?;
    let mut native = Native::new(config, machine, stop, store)?;
    let mut fault = None;
    for member in page.rows {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let now = now();
        let eligible = team.home == machine
            && (team.status == TeamStatus::Running || planning && member.id == team.lead)
            && member.state == MemberState::Active;
        let bound = member.session.as_ref().filter(|_| member.agent != "dsh");
        let binding = bound.and_then(|_| native.binding(team_id, &member.id).ok());
        let observation = if let Some((binding, _)) = &binding {
            native.reset();
            let result = engine::step(&mut native, binding, epoch, now);
            let observation = native.observation.take();
            if !matches!(observation, Some(Err(_))) {
                if let Err(error) = result {
                    fault = Some(error);
                }
            }
            observation
        } else {
            None
        };
        let active_binding = eligible
            && binding.as_ref().is_some_and(|(binding, _)| {
                policy::stored(
                    binding,
                    &Facts {
                        team: team.clone(),
                        member: member.clone(),
                        binding: binding.clone(),
                        preview: binding.machine == machine,
                        planning_paused: !planning,
                    },
                )
                .is_ok()
            });
        let updated = if active_binding {
            if let (Some((binding, _)), Some(observation)) = (&binding, &observation) {
                apply_observation(
                    cards,
                    native.store,
                    (&team, &member, binding),
                    observation,
                    now,
                    &mut report,
                )
            } else {
                Ok(())
            }
        } else if !eligible || bound.is_none() || binding.is_some() {
            cards
                .confirmation_observation(native.store, &team, &member, None, now, |_| {})
                .map_err(|e| e.message)
        } else {
            // A transient provenance read keeps the prior card and timer.
            Ok(())
        };
        if let Err(error) = updated {
            fault = Some(error);
        }
    }
    cursor.members = page.next;
    if cursor.members.is_none() {
        cursor.team = None;
    }
    fault.map_or(Ok(()), Err)
}

fn apply_observation(
    cards: &mut actor::Runtime,
    store: &mut Store,
    scope: (&Team, &Member, &Binding),
    observation: &Result<()>,
    now: i64,
    report: impl FnOnce(&str),
) -> Result<()> {
    let (team, member, binding) = scope;
    let stamp = observation.as_ref().err().map(|_| {
        actor::provider_binding_stamp(
            &binding.launch,
            &binding.session,
            &binding.run,
            &binding.conversation,
        )
    });
    let fault = stamp
        .as_deref()
        .zip(observation.as_ref().err().map(String::as_str));
    cards
        .confirmation_observation(store, team, member, fault, now, report)
        .map_err(|e| e.message)
}

#[cfg(test)]
mod tests;
