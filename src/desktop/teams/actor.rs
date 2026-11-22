//! Bounded team work, separate from member creation. A durable delivery row
//! never grants permission to send: only this tick owns its original call.
#![allow(dead_code)] // Parts A/B are wired into the worker in this package.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod auxiliary;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod availability;
mod cancel;
mod delivery;
mod finish;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod fleet;
mod integration;
mod ledger;
mod lead_queue;
mod maintenance;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) mod native;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod native_work;
mod non_delivery;
mod observe;
mod ports;
mod presence;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod sweep_items;
pub(crate) use presence::presence;
mod reply;
mod reports;
mod retry;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use retry::provider_binding_stamp;
mod schedule;
mod tasks;
mod publication;
mod rework;
mod project_limit;
mod terminal;
mod timing;
#[cfg(test)]
mod tests;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use tests::producers::failing_service_passes;
/// The actor ledger of a state directory, created as the service creates
/// it, and a plain connection to it for a test's own writes.
#[cfg(test)]
pub(in crate::desktop::teams) fn test_ledger(state: &std::path::Path) -> rusqlite::Connection {
    drop(ledger::Ledger::open(state).unwrap());
    rusqlite::Connection::open(state.join("teams/actor-v1.sqlite3")).unwrap()
}
use crate::teams::{
    ops::{self, Actor, Operation, Request},
    store::{Store, TeamRow},
};
use ledger::Row;
pub(crate) use ledger::Ledger;
use ports::{Ports, Result};
use std::{collections::BTreeMap, path::Path};
use zerus_team_plan::{model::*, problem::code, Problem};

pub(super) struct Runtime {
    ledger: Ledger,
    machine: String,
    epoch: String,
    maintenance: Option<crate::teams::store::Cursor>,
    maintenance_team: Option<(String, String)>,
    row_failure: bool,
    provider_stops: BTreeMap<(String, String), String>,
    hints: BTreeMap<String, observe::Hint>,
    cancel_after: String,
    /// The orphan pass's cursor over the service items.
    items_after: super::items::Key,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    auxiliary: auxiliary::State,
}
impl Runtime {
    pub fn open(state: &Path, machine: &str) -> Result<Self> {
        if !crate::teams::enabled(state) {
            return Err(Problem::new(code::TEAMS_INACTIVE, "Teams preview is off"));
        }
        let epoch = uuid::Uuid::new_v4().to_string();
        Ok(Self {
            ledger: Ledger::open(state)?,
            machine: machine.into(),
            epoch: epoch.clone(),
            maintenance: None,
            maintenance_team: None,
            row_failure: false,
            provider_stops: BTreeMap::new(),
            hints: BTreeMap::new(),
            cancel_after: String::new(),
            items_after: Default::default(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            auxiliary: auxiliary::State::new(state, &epoch),
        })
    }
    pub fn begin_pass(&mut self) {
        self.hints.clear();
    }
    pub fn tick(
        &mut self,
        store: &mut Store,
        ports: &mut impl Ports,
        id: &str,
        now: i64,
        safety: bool,
    ) -> Result<()> {
        self.row_failure = false;
        if ports.stopped() {
            return Ok(());
        }
        let Some(team) = store.team(id)? else {
            return Ok(());
        };
        if team.home != self.machine {
            return Ok(());
        }
        self.non_deliveries(store, &team, now)?;
        let planning_pass = team.status == TeamStatus::Planning;
        if !matches!(team.status, TeamStatus::Running | TeamStatus::Paused) {
            self.publications(store, ports, &team, now)?;
        }
        let Some(team) = store.team(id)? else {
            return Ok(());
        };
        if team.status == TeamStatus::Planning {
            // Planning has no task/reopen/stage/integration work. Its durable
            // goal uses the existing lead inbox and witnessed doorbell route.
            self.observe(store, ports, &team, now, safety)?;
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if store.planning(id)?.is_some_and(|p| !p.paused) {
                self.team_entry(store, ports, &team, now)?;
            }
            return Ok(());
        }
        if !matches!(team.status, TeamStatus::Running | TeamStatus::Paused) {
            let settled = self.terminal_deliveries(store, &team, now);
            self.for_row(|_| settled)?;
            return Ok(());
        }
        self.observe(store, ports, &team, now, safety)?;
        if ports.stopped() {
            return Ok(());
        }
        self.cancels(store, ports, &team, now)?;
        self.reports(store, ports, &team, now)?;
        if ports.stopped() {
            return Ok(());
        }
        if !planning_pass {
            self.publications(store, ports, &team, now)?;
        }
        self.stage_tasks(store, ports, &team, now)?;
        if store
            .team(id)?
            .is_some_and(|t| t.status == TeamStatus::Running)
        {
            self.lead_queue(store, &team, now)?;
            self.integrations(store, ports, &team, now)?;
            if ports.stopped() {
                return Ok(());
            }
            self.finish(store, ports, &team, now)?;
            self.recover(store, ports, &team, now)?;
            self.reopens(store, ports, &team, now)?;
            self.handouts(store, ports, &team, now)?;
        } else {
            self.recover_merges(store, ports, &team, now)?;
            self.recover(store, ports, &team, now)?;
            // A paused team hands nothing out, so it needs no member now.
            self.unreadable_members(store, &team, &schedule::Needs::default(), now)?;
        }
        self.phase_completion(store, &team, now)?;
        self.cleanup_metadata(store, &team, now)?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let lead = self.team_entry(store, ports, &team, now);
            let answers = self.answers_entry(store, ports, &team, now);
            self.for_row(|_| lead.and(answers))?;
        }
        Ok(())
    }

    pub fn page(
        &mut self,
        store: &mut Store,
        ports: &mut impl Ports,
        teams: &[TeamRow],
        now: i64,
        safety: bool,
    ) -> Result<()> {
        self.provider_stops.clear();
        self.prepare(teams)?;
        let mut fault = None;
        for team in teams {
            if ports.stopped() {
                return Ok(());
            }
            if let Err(error) = self.tick(store, ports, &team.id, now, safety) {
                if !self.row_failure || error.code == ledger::CAPACITY {
                    fault = Some(error);
                }
            }
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Err(error) = self.terminal_entry(store, ports, now) {
            fault = Some(error);
        }
        if let Err(error) = self.maintain(store) {
            fault = Some(error);
        }
        fault.map_or(Ok(()), Err)
    }
    fn for_row<T>(&mut self, apply: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.row_failure = true;
        let result = apply(self);
        if result.is_ok() {
            self.row_failure = false;
        }
        result
    }
    pub fn maintain(&mut self, store: &Store) -> Result<()> {
        self.prune_items(store)?;
        let mut cleared = false;
        for _ in 0..16 {
            if self.maintenance_team.is_none() {
                let page = store.teams(1, self.maintenance.as_ref())?;
                self.maintenance = page.next;
                let Some(team) = page.rows.first() else { break };
                if matches!(
                    team.status,
                    TeamStatus::Stopped | TeamStatus::Archived | TeamStatus::Finished
                ) {
                    self.maintenance_team = Some((team.id.clone(), String::new()));
                } else if self.maintenance.is_none() {
                    break;
                } else {
                    continue;
                }
            }
            // One page of the closed keys per pass, for the current terminal
            // team (zerus-63rru.32).
            if !cleared {
                cleared = true;
                self.clear_closed_keys(store)?;
            }
            if !self.prune_terminal_rows(store)? || self.maintenance.is_none() {
                break;
            }
        }
        Ok(())
    }
    fn put<T: serde::Serialize>(&mut self, team: &str, key: &str, value: &T) -> Result<Row> {
        let data = serde_json::to_value(value).map_err(crate::teams::store::storage)?;
        if let Some(mut row) = self.ledger.get(team, key)? {
            if row.data != data {
                row.data = data;
                self.ledger.save(&mut row)?;
            }
            Ok(row)
        } else {
            self.ledger.reserve(team, key, data)
        }
    }
    fn load<T: serde::de::DeserializeOwned>(&self, team: &str, key: &str) -> Result<Option<T>> {
        self.ledger
            .get(team, key)?
            .map(|r| serde_json::from_value(r.data).map_err(crate::teams::store::storage))
            .transpose()
    }
}
fn request(store: &Store, team: &str, operation: Operation) -> Result<(Actor, Request)> {
    let t = store
        .team(team)?
        .ok_or_else(|| problem("Team disappeared"))?;
    Ok((
        Actor::Service {
            person: t.owner,
            team: team.into(),
        },
        Request {
            request_id: uuid::Uuid::new_v4().to_string(),
            team: team.into(),
            expected_revision: t.revision,
            operation,
        },
    ))
}
fn write(store: &mut Store, team: &str, op: Operation, now: i64) -> Result<ops::Reply> {
    let (actor, request) = request(store, team, op)?;
    ops::apply(store, &actor, &request, now)
}
fn problem(text: &str) -> Problem {
    Problem::new(code::INVALID_STATE, text)
}
fn cut(s: &str, max: usize) -> String {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1
    }
    s[..end].into()
}
