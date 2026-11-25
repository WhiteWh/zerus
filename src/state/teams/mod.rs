//! WhiteWh fork: agent teams. A lead session plans; members (any agent,
//! model and computer) work through dependency-ordered tasks that a dispatcher
//! hands out. See docs/superpowers/specs/2026-10-24-agent-teams-core-design.md.
pub(crate) mod broker;
pub(crate) mod artifacts;
pub(crate) mod catalog;
pub(crate) mod commands;
pub(crate) mod context;
pub(crate) mod decisions;
pub(crate) mod dispatcher;
pub(crate) mod dispatcher_rebalance;
pub(crate) mod baseline;
pub(crate) mod estimates;
pub(crate) mod handoff;
pub(crate) mod inbox;
pub(crate) mod environment;
pub(crate) mod isolation;
pub(crate) mod lead_stall;
pub(crate) mod lead_plan;
pub(crate) mod look;
pub(crate) mod model;
pub(crate) mod models;
pub(crate) mod ops;
pub(crate) mod outbox;
pub(crate) mod owner_queue;
pub(crate) mod owner_waiting;
pub(crate) mod terminal_prompt;
pub(crate) mod owner_word;
pub(crate) mod pipeline;
pub(crate) mod plan;
pub(crate) mod run_results;
pub(crate) mod profile;
pub(crate) mod lore_sync;
pub(crate) mod owner_steps;
pub(crate) mod park;
pub(crate) mod wait_loops;
pub(crate) mod pulse_worker;
pub(crate) mod pulses;
pub(crate) mod shadow;
pub(crate) mod rights;
pub(crate) mod safe_actions;
pub(crate) mod sandbox_scope;
pub(crate) mod scratch;
pub(crate) mod secrets;
pub(crate) mod spans;
pub(crate) mod strict;
pub(crate) mod stages;
pub(crate) mod store;
// WhiteWh fork: a remote member's report when the lead's computer is out of reach.
pub(crate) mod member_outbox;
pub(crate) mod templates;
pub(crate) mod test_queue;
pub(crate) mod test_results;
pub(crate) mod tick_clock;
pub(crate) mod test_runners;
pub(crate) mod merge_queue;
pub(crate) mod timing;
pub(crate) mod usage_sampler;
pub(crate) mod waiting;
pub(crate) mod wake;
pub(crate) mod remote_catalog;
pub(crate) mod working;
pub(crate) mod working_mode;
pub(crate) mod playbook;
pub(crate) mod doorbell;

use std::collections::BTreeSet;

/// U43/N3: the sessions of non-lead members of active teams (Planning, Running,
/// Paused), keyed by (host, name) and only for sessions on this computer. hgs
/// mutes such a member by default unless the owner recorded a notification
/// choice, so Android and every desktop here follow one preference. A member on
/// another computer keeps its notifications there: its hgs has no team record,
/// which docs/reference.md states. A session that leads any active team is
/// never muted by this default.
pub(crate) fn muted_member_sessions() -> BTreeSet<(String, String)> {
    let Ok(store) = store::Store::open(false) else { return BTreeSet::new() };
    let Ok(teams) = store.active_teams(200) else { return BTreeSet::new() };
    let mut members = BTreeSet::new();
    let mut leads = BTreeSet::new();
    for team in teams {
        for member in &team.members {
            // A session on another computer is not this hgs's to mute.
            if !member.computer.is_empty() {
                continue;
            }
            let Some(session) = member.session.as_ref().filter(|session| !session.name.is_empty() && session.host.is_empty()) else { continue };
            let key = (session.host.clone(), session.name.clone());
            if member.id == team.lead {
                leads.insert(key);
            } else {
                members.insert(key);
            }
        }
    }
    members.retain(|key| !leads.contains(key));
    members
}

/// P4: the team an agent session belongs to names its project: the team's
/// `project_id`, else its project label, else its folder. A member worktree is
/// not a catalog session, so `hgs task` reads this before giving up.
pub(crate) fn project_for_session(session: &str) -> Option<(String, String, String)> {
    let store = store::Store::open(false).ok()?;
    let teams = store.active_teams(1000).ok()?;
    for team in teams {
        let matches = team
            .members
            .iter()
            .any(|member| member.session.as_ref().is_some_and(|found| found.name == session));
        if matches {
            return Some((team.project_id, team.project, team.folder));
        }
    }
    None
}
