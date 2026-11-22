//! A single worker advances one fixed original at a time. Durable state alone
//! cannot populate `admitted`: a replacement runtime only reads and reports
//! an unfinished original. It never executes its remaining steps.
use super::{
    ledger::{BranchPlan, Ledger, Row, State, BRANCH},
    ports::{Effect, Effects, LaunchRead, Plan, Result},
};
use crate::teams::{
    ops::{self, Actor, Operation, Request},
    store::Store,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use zerus_team_plan::{
    accounts::pick_account, catalog::AccountChoice, model::*, problem::code, Problem,
};

pub(super) struct Runtime {
    state: PathBuf,
    machine: String,
    epoch: String,
    ledger: Option<Ledger>,
    admitted: Option<(String, String)>,
    accounts: BTreeMap<String, Result<Vec<zerus_team_plan::accounts::AccountUsage>>>,
    selected: Option<(String, String)>,
    prune_after: String,
}
mod branch;
mod budget;
mod card;
mod edits;
mod effects;
mod member;
mod retry;
#[cfg(test)]
mod tests;
impl Runtime {
    pub fn open(state: &Path, machine: &str, epoch: &str) -> Result<Self> {
        Ok(Self {
            state: state.into(),
            machine: machine.into(),
            epoch: epoch.into(),
            ledger: None,
            admitted: None,
            accounts: BTreeMap::new(),
            selected: None,
            prune_after: String::new(),
        })
    }
    /// One bounded transition; `true` permits the worker to advance another
    /// step. NotStarted yields until the next external wakeup/safety pass.
    pub fn tick(
        &mut self,
        store: &mut Store,
        effects: &mut impl Effects,
        id: &str,
        now: i64,
    ) -> Result<bool> {
        if !crate::teams::enabled(&self.state) {
            self.admitted = None;
            return Ok(false);
        }
        if self.admitted.as_ref().is_some_and(|(team, _)| team != id) {
            self.admitted = None;
        }
        self.selected = None;
        let result = self.tick_inner(store, effects, id, now);
        if !matches!(result, Ok(true)) {
            self.admitted = None;
        }
        if let Err(error) = &result {
            if super::ledger::is_budget(error) {
                self.notify_budget(store, id, error.clone(), now)?;
                return Ok(false);
            } else {
                self.record_failure(store, id, error.clone(), now)?;
            }
        }
        result
    }
    fn tick_inner(
        &mut self,
        store: &mut Store,
        effects: &mut impl Effects,
        id: &str,
        now: i64,
    ) -> Result<bool> {
        store.refresh()?;
        let Some(team) = store.team(id)? else {
            return Ok(false);
        };
        if !store.launch_allowed(&team, &team.lead)? || team.home != self.machine {
            if matches!(team.status, TeamStatus::Stopped | TeamStatus::Archived) {
                self.note_late(&team, now)?;
            }
            self.admitted = None;
            return Ok(false);
        }
        if self.ledger.is_none() {
            self.ledger = Some(Ledger::open(&self.state)?);
        }
        if !self.budget_due(store, &team, now)? {
            return Ok(false);
        }
        if team.status == TeamStatus::Running
            && team.settings.integration == Integration::Branch
            && !self.branch(store, effects, &team, now)?
        {
            return Ok(self
                .ledger()
                .get(&team.id, BRANCH)?
                .is_some_and(|r| r.state == State::BranchIntent && self.owns(&r)));
        }
        self.selected = None;
        let lead = store
            .member(id, &team.lead)?
            .ok_or_else(|| invalid("Lead member is missing"))?;
        if lead.session.is_none() {
            return self.member(store, effects, &team, &lead, None, now);
        }
        if team.status == TeamStatus::Planning {
            return Ok(false);
        }
        // Caps bound the roster at 201 including the lead. Read pages rather
        // than a journal, session catalog or a serialized whole-team value.
        let mut cursor = None;
        for _ in 0..2 {
            self.selected = None;
            let page = store.members(id, 200, cursor.as_ref())?;
            for member in page.rows {
                if member.id == team.lead {
                    continue;
                }
                if member.state == MemberState::Removed {
                    self.note_removed(store, &team, &member, now)?;
                    continue;
                }
                if self.member(store, effects, &team, &member, lead.session.clone(), now)? {
                    return Ok(true);
                }
            }
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        Ok(false)
    }
    pub fn record_health(&mut self, problem: &Problem, now: i64) -> Result<()> {
        if self.ledger.is_none() {
            self.ledger = Some(Ledger::open(&self.state)?);
        }
        let code = if super::ledger::is_notice_budget(problem) {
            1
        } else {
            2
        };
        self.ledger().write_health(Some(code), now)
    }
    pub fn row_failure(&self) -> bool {
        self.selected.is_some()
    }
    pub fn clear_health(&mut self) -> Result<()> {
        self.clear_health_code(super::health::WORKER_FAULT)
    }
    pub(super) fn clear_notification_overflow(&mut self) -> Result<()> {
        self.clear_health_code(super::health::NOTIFICATION_OVERFLOW)
    }
    fn clear_health_code(&mut self, code: &str) -> Result<()> {
        if super::health(&self.state).is_some_and(|previous| previous.code == code) {
            if self.ledger.is_none() {
                self.ledger = Some(Ledger::open(&self.state)?);
            }
            self.ledger().write_health(None, 0)?;
        }
        Ok(())
    }
    pub fn begin_pass(&mut self) {
        self.accounts.clear();
    }
    pub fn maintain(&mut self, store: &Store) -> Result<()> {
        if let Some(ledger) = self.ledger.as_mut() {
            ledger.prune(store, &mut self.prune_after)?;
        }
        Ok(())
    }
    pub fn release(&mut self) {
        self.admitted = None;
    }
    pub fn busy(&self) -> bool {
        self.admitted.is_some()
    }
    fn ledger(&mut self) -> &mut Ledger {
        self.ledger
            .as_mut()
            .expect("opened after running team read")
    }
    fn save(&mut self, row: &mut Row, now: i64) -> Result<()> {
        self.ledger().save(row, now)
    }
    fn owns(&self, row: &Row) -> bool {
        self.admitted
            .as_ref()
            .is_some_and(|(team, original)| team == &row.team && original == &row.original)
    }
    fn take(&mut self, row: &Row) -> Result<()> {
        if !self.owns(row) {
            return Err(uncertain("Original admission is unavailable"));
        }
        self.admitted.take();
        Ok(())
    }
    fn block(&mut self, row: &mut Row, problem: Problem, now: i64) -> Result<()> {
        if row.state != State::Blocked {
            row.state = State::Blocked;
            row.problem = Some(problem);
            self.admitted = None;
            self.save(row, now)?;
        }
        Ok(())
    }
}
/// One intended write through the store, with one request ID kept across
/// every retry and a restart, persisted with the row (zerus-63rru.91, the
/// task intake's and the actor's receipt rule). A success — including a
/// replay the store answers from its saved receipt — clears the reservation,
/// because the intent is settled and the row's next intent needs its own
/// ID; a stale revision or another definitive answer also clears it, and
/// only a storage failure keeps it, its commit unknown. A kept reservation
/// whose operation differs from the current intent is replayed first to
/// settle its unknown result, then cleared and the current intent is
/// reserved and applied in the same call. If the process dies after the
/// store's commit but before the row's save, the reservation is gone with
/// the row; what then keeps the replay safe is the callers' pre-checks (the
/// member's session is already the key, the card is no longer open), which
/// see the committed write and make no second one — the persisted ID's own
/// guarantee is the answer replay, never the crash.
fn apply(
    store: &mut Store,
    team: &Team,
    operation: Operation,
    reserved: &mut Option<Request>,
    now: i64,
) -> Result<()> {
    let actor = Actor::Service {
        person: team.owner.clone(),
        team: team.id.clone(),
    };
    if let Some(kept) = reserved.as_ref().filter(|r| r.operation != operation) {
        // Settle the kept intent first: a success or a definitive answer
        // ends it; an unknown result leaves it reserved and blocks the
        // current intent until the next pass. A stale revision means the
        // kept write never applied: re-reserve its own operation against
        // the current revision and apply it now, so the kept intent is not
        // dropped and the current intent follows in the same call.
        match ops::apply(store, &actor, kept, now) {
            Ok(_) => *reserved = None,
            Err(p) if p.code == code::STORAGE_FAILED => return Err(p),
            Err(p) if p.code == code::STALE_REVISION => {
                let current = store
                    .team(&team.id)?
                    .ok_or_else(|| invalid("Team disappeared"))?;
                let fresh = Request {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    team: team.id.clone(),
                    expected_revision: current.revision,
                    operation: kept.operation.clone(),
                };
                match ops::apply(store, &actor, &fresh, now) {
                    Ok(_) => *reserved = None,
                    Err(p) if p.code == code::STORAGE_FAILED => {
                        *reserved = Some(fresh);
                        return Err(p);
                    }
                    Err(_) => *reserved = None,
                }
            }
            Err(_) => *reserved = None,
        }
    }
    if reserved.is_none() {
        let current = store
            .team(&team.id)?
            .ok_or_else(|| invalid("Team disappeared"))?;
        *reserved = Some(Request {
            request_id: uuid::Uuid::new_v4().to_string(),
            team: team.id.clone(),
            expected_revision: current.revision,
            operation,
        });
    }
    match ops::apply(store, &actor, reserved.as_ref().expect("reserved above"), now) {
        Ok(_) => {
            *reserved = None;
            Ok(())
        }
        Err(p) if p.code == code::STORAGE_FAILED => Err(p),
        Err(p) => {
            *reserved = None;
            Err(p)
        }
    }
}
pub(super) fn invalid(text: &str) -> Problem {
    Problem::new(code::INVALID_STATE, text)
}
pub(super) fn uncertain(text: &str) -> Problem {
    Problem::new(code::DELIVERY_UNCERTAIN, text)
}

fn as_uncertain(mut p: Problem) -> Problem {
    p.code = code::DELIVERY_UNCERTAIN.into();
    p
}
