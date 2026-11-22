//! Original local CLI effects with exact stored admissions.
//! Occupancy is a scheduling hint read just before admission, outside the
//! process-wide spawn turn because it starts child readers. A task a person
//! assigns in the remaining race can reach the member alongside this hand-out.
//! The exact current fleet row is checked last inside the store admission.

mod tasks;
#[cfg(test)]
#[allow(unused_imports)]
pub(super) use tasks::{clear_request, stage_arguments, stage_request, task_reply};
mod admission;
mod fresh;
#[cfg(test)]
mod fresh_fixture;
mod cancels;
use super::*;
use crate::{
    config::Config,
    desktop::teams::child::{self, Ended},
    teams::ops::HandoutOutcome,
};
#[cfg(test)]
pub(super) use cancels::answer as cancel_answer;
use serde_json::{json, Value};
use std::{
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};
pub(in crate::desktop::teams) struct Native {
    pub config: Config,
    pub machine: String,
    pub stop: Arc<AtomicBool>,
    pub epoch: String,
}
impl Native {
    fn read(&self, args: &[String], deadline: Instant) -> Result<Value> {
        match child::run(&self.config, args, None, None, false, deadline, &self.stop) {
            Ended::Exited { code: 0, output } => {
                serde_json::from_slice(&output).map_err(|_| problem("Invalid bounded CLI read"))
            }
            _ => Err(problem("Bounded CLI read is unavailable")),
        }
    }
    fn project_inventory(&self) -> Result<Value> {
        #[cfg(test)]
        if let Some(value) = TEST_INVENTORY.with(|s| s.borrow().clone()) {
            return Ok(value);
        }
        crate::swarm::organization(&self.config, false)
            .map_err(|_| problem("Project inventory unavailable"))
    }
    pub(super) fn unoccupied(&self, m: &Member) -> Result<bool> {
        let s = m
            .session
            .as_ref()
            .ok_or_else(|| problem("Member has no session"))?;
        let organization = self.project_inventory()?;
        if organization["node_id"] != self.machine {
            return Err(problem("Project machine changed"));
        }
        let projects = organization["organization"]["projects"]
            .as_array()
            .ok_or_else(|| problem("Project inventory unavailable"))?;
        if projects.len() > 128 {
            return Err(problem("Task occupancy project bound exceeded"));
        }
        let until = Instant::now() + Duration::from_secs(3);
        for project in projects {
            if Instant::now() >= until {
                return Err(problem("Task occupancy read deadline"));
            }
            let id = project["id"]
                .as_str()
                .ok_or_else(|| problem("Invalid project identity"))?;
            let args = occupancy_arguments(id, &s.session_id);
            let r = self.read(&args, until)?;
            if r["damaged_total"].as_u64() != Some(0) {
                return Err(problem("Task occupancy is unknown"));
            }
            let tasks = r["tasks"]
                .as_array()
                .ok_or_else(|| problem("Task occupancy is unknown"))?;
            if !tasks.is_empty() {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn command(
        &self,
        team: &Team,
        node: &Node,
        member: Option<&Member>,
        id: &str,
        action: &str,
        decision: Option<&crate::teams::store::Decision>,
    ) -> Result<Ended> {
        let Some(task) = node.task.as_ref() else {
            return Ok(Ended::NotStarted);
        };
        if let Some(member) = member {
            if !self.unoccupied(member)? {
                return Err(problem("The member has another active task"));
            }
        }
        // Native terminal reads cannot run inside the final spawn admission.
        // The child still checks the native gate again before offering input.
        let first = match member {
            Some(member) => self.first_witness(team, member)?,
            None => None,
        };
        let args = task_arguments(task, action);
        let input = task_request(team, node, member, id);
        #[cfg(test)]
        capture_child(&args, &input);
        let input = input.to_string();
        let mut retry = None;
        let ended = child::run_guarded(
            &self.config,
            &args,
            Some(input.as_bytes()),
            Some(id),
            false,
            Instant::now() + Duration::from_secs(30),
            &self.stop,
            || {
                self.admit(team, node, member, id, decision, first.as_ref())
                    .inspect_err(|e| {
                        if e.code == crate::teams::store::actor_reads::ADMISSION_BUSY {
                            retry = Some(e.clone());
                        }
                    })
            },
        );
        if let Some(error) = retry {
            return Err(error);
        }
        Ok(ended)
    }
}
impl Ports for Native {
    fn recover_delivery(&mut self, team: &Team, node: &Node, member: &Member, id: &str) -> Result<Option<HandoutOutcome>> {
        let task = node.task.as_ref().ok_or_else(|| problem("Delivery has no task"))?;
        let mut args = task_arguments(task, "team-deliver");
        args.push("--recover-only".into());
        let input = task_request(team, node, Some(member), id).to_string();
        // Recovery is allowed while the member is working. The command
        // reads only its retained request, and rechecks the effect before
        // an Absent answer may offer that exact input.
        let ended = child::run_guarded(&self.config, &args, Some(input.as_bytes()), Some(id), false,
            Instant::now() + Duration::from_secs(30), &self.stop, || {
                let store = Store::open(&self.config.state)?.admission()?;
                let current = store.team(&team.id)?.ok_or_else(|| problem("Recovery team disappeared"))?;
                if current.home != self.machine || current.owner != team.owner || current.project != team.project
                    || current.status != TeamStatus::Running || !store.effect_reserved(&team.id, id)?
                    || store.node(&team.id, node.id)?.as_ref().and_then(|n| n.task.as_ref()) != Some(task) {
                    return Err(problem("Delivery recovery admission changed"));
                }
                Ok(store)
            });
        if let Ended::Exited { code: 0, output } = &ended {
            if let Ok(value) = serde_json::from_slice::<Value>(output) {
                if value["id"] == id && value["schema"] == 1 && value["state"] != "pending" {
                    return Ok(Some(decode_delivery(output, id)));
                }
            }
        }
        Ok(None)
    }
    fn cancels(
        &mut self,
        store: &Store,
        team: &str,
        after: u32,
    ) -> Result<Vec<super::cancel::Pending>> {
        Self::pending_cancels(store, team, after)
    }
    fn cancel(
        &mut self,
        team: &Team,
        pending: &super::cancel::Pending,
        original: &str,
    ) -> Result<super::cancel::Answer> {
        self.run_cancel(team, pending, original)
    }
    fn record_cancel(
        &mut self,
        store: &mut Store,
        team: &Team,
        fact: &super::cancel::Fact,
        now: i64,
    ) -> Result<()> {
        Self::save_cancel(store, team, fact, now)
    }

    fn clock_ms(&self, _: i64) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
    fn team_timers(
        &mut self,
        state: &mut super::auxiliary::State,
        store: &mut Store,
        team: &Team,
        now: i64,
    ) -> Result<()> {
        state.team(self, store, team, now)
    }
    fn answer_timers(
        &mut self,
        state: &mut super::auxiliary::State,
        store: &mut Store,
        team: &Team,
        now: i64,
    ) -> Result<()> {
        state.answers(self, store, team, now)
    }
    fn terminal_timers(
        &mut self,
        state: &mut super::auxiliary::State,
        store: &mut Store,
        now: i64,
    ) -> Result<()> {
        state.terminal(self, store, now)
    }
    fn planned_task(
        &mut self,
        team: &Team,
        request: &crate::teams::store::publication::PlannedTaskRequest,
        clear: bool,
    ) -> Result<super::ports::TaskReply> {
        self.planned_task_child(team, request, clear)
    }
    fn stage(
        &mut self,
        team: &Team,
        effect: &crate::teams::store::StageTaskEffect,
        original: &str,
    ) -> Result<super::ports::TaskReply> {
        self.create_task(team, effect, original)
    }
    fn clear_stage(
        &mut self,
        team: &Team,
        keyed: &crate::teams::store::timers::KeyedTask,
        original: &str,
    ) -> Result<super::ports::TaskReply> {
        self.clear_task(team, keyed, original)
    }
    fn stopped(&self) -> bool {
        self.stop.load(std::sync::atomic::Ordering::Acquire)
    }
    fn changes(
        &mut self,
        project: &str,
        generation: &str,
        offset: u64,
    ) -> Result<zerus_replica::Changes> {
        Ok(zerus_replica::ProjectStore::read_changes_since(
            &self.config.state.join("replica"),
            project,
            generation,
            offset,
        ))
    }
    fn observe(&mut self, _: &mut Store, task: &TaskRef) -> Result<()> {
        crate::teams::tasks::observe(&self.config.state, task)
    }
    fn timing_supported(&self) -> bool {
        true
    }
    fn observe_timing(
        &mut self,
        _: &mut Store,
        task: &TaskRef,
        epoch: &str,
        generation: Option<String>,
        offset: Option<u64>,
    ) -> Result<Option<crate::teams::tasks::timing::TimingRead>> {
        crate::teams::tasks::timing::observe(&self.config.state, task, epoch, generation, offset)
    }
    fn report(&mut self, task: &TaskRef) -> Result<Option<String>> {
        crate::teams::tasks::report(&self.config.state, task)
    }
    fn free(&mut self, team: &Team, m: &Member) -> Result<bool> {
        Ok(self.availability(team, m)? == ports::Availability::Free)
    }
    /// One read: the fleet row's answer, then the session's task index for a
    /// member that row calls free.
    fn row_stamp(&mut self, team: &Team, m: &Member) -> Result<Option<String>> {
        let Some(session) = m.session.as_ref() else {
            return Ok(None);
        };
        if session.machine != self.machine {
            return Ok(None);
        }
        Ok(
            super::availability::record(&self.config, &self.epoch, &team.id, m, false)?
                .map(|row| super::fleet::stamp(&row)),
        )
    }
    fn availability(&mut self, team: &Team, m: &Member) -> Result<ports::Availability> {
        let Some(session) = m.session.as_ref() else {
            return Ok(ports::Availability::Busy);
        };
        if session.machine != self.machine {
            return Ok(ports::Availability::Busy);
        }
        match super::availability::with_first(&self.config, &self.epoch, &team.id, m, true,
            |row| self.first_on(m, row).is_some())? {
            ports::Availability::Free if !self.unoccupied(m)? => Ok(ports::Availability::Busy),
            answer => Ok(answer),
        }
    }
    fn held_availability(
        &mut self,
        store: &mut Store,
        team: &Team,
        member: &Member,
        effect: &crate::teams::store::EffectIntent,
        confirmation: Option<&ops::ProviderConfirmation>,
        now: i64,
    ) -> Result<(ports::Availability, Option<String>)> {
        if !member
            .session
            .as_ref()
            .is_some_and(|session| session.machine == self.machine)
        {
            return Ok((ports::Availability::Busy, None));
        }
        let mut stopped = None;
        let answer = super::availability::held(
            &self.config,
            &self.epoch,
            &team.id,
            member,
            |record, failure| {
                match crate::teams::ops::apply_provider_stop(
                    store,
                    team,
                    effect,
                    ops::ProviderStopProof {
                        member,
                        record,
                        stop: failure,
                        confirmation,
                    },
                    now,
                ) {
                    Ok(_) => {
                        stopped = Some(super::provider_binding_stamp(
                            &record.launch_id,
                            &record.session_id,
                            &record.run_id,
                            &record.conversation_id,
                        ));
                    }
                    Err(e)
                        if e.code == crate::teams::ops::PROVIDER_STOP_CHANGED
                            || e.code == zerus_team_plan::problem::code::STALE_REVISION => {}
                    Err(e) => return Err(e),
                }
                Ok(())
            },
            |row| self.first_on(member, row).is_some(),
        )?;
        Ok((answer, stopped))
    }
    fn repository(&mut self, team: &Team) -> Result<String> {
        self.work_repository(team)
    }
    fn reported(
        &mut self,
        team: &Team,
        member: &Member,
    ) -> Result<crate::teams::integrate::Reported> {
        self.work_reported(team, member)
    }
    fn descends(&mut self, plan: &super::ports::MergePlan) -> Result<bool> {
        self.work_descends(plan)
    }
    fn contains(&mut self, plan: &super::ports::MergePlan) -> Result<Option<String>> {
        self.work_contains(plan)
    }
    fn merge(&mut self, plan: &super::ports::MergePlan) -> super::ports::MergeResult {
        self.work_merge(plan)
    }
    fn finish(
        &mut self,
        team: &Team,
        node: &Node,
        decision: &crate::teams::store::Decision,
        id: &str,
    ) -> Result<super::ports::Finish> {
        self.done(team, node, decision, id)
    }
    fn deliver(
        &mut self,
        team: &Team,
        node: &Node,
        member: &Member,
        id: &str,
    ) -> ports::DeliveryResult {
        match self.command(team, node, Some(member), id, "team-deliver", None) {
            Ok(Ended::NotStarted) if member.agent == "dsh" => ports::DeliveryResult::NotStarted {
                reason: "The hosted delivery child did not start".into(),
            },
            // Admission proved that no child started. A fresh member's
            // first-send gate keeps that proof pending under its named wait;
            // a final or unknown child result never enters this branch.
            Ok(Ended::NotStarted) if matches!(self.availability(team, member),
                Ok(ports::Availability::Unreadable(ports::Fact::FirstMessage))) =>
                ports::DeliveryResult::NotStarted {
                    reason: "The member is waiting for its first-message readiness".into(),
                },
            Ok(ended) if member.agent == "dsh" => {
                let confirmed = match &ended {
                    Ended::Exited { code: 0, output } => serde_json::from_slice::<Value>(output).is_ok_and(|v|
                        v["schema"] == 1 && v["id"] == id && matches!(v["state"].as_str(), Some("submitted" | "refused" | "uncertain" | "cutting_over" | "stale" | "not_started"))),
                    _ => false,
                };
                if confirmed { delivery_result(ended, id) } else { ports::DeliveryResult::ReadLater }
            }
            Ok(ended) => delivery_result(ended, id),
            Err(error) => ports::DeliveryResult::NotStarted {
                reason: error.message,
            },
        }
    }
}
pub(super) fn delivery_result(ended: Ended, id: &str) -> ports::DeliveryResult {
    if let Ended::Exited { code: 0, output } = &ended {
        if let Ok(v) = serde_json::from_slice::<Value>(output) {
            if v["schema"] == 1 && v["id"] == id {
                match (v["state"].as_str(), v["error"].as_str()) {
                    (Some("stale"), Some(_)) => return ports::DeliveryResult::Stale,
                    // The member's terminal holds input, by the answer's
                    // typed cause, never its sentence (zerus-63rru.79).
                    (Some("refused"), Some(reason))
                        if v["cause"] == crate::tasks::TERMINAL_WAITING =>
                    {
                        let said = super::cancel::reason::spoken(&cut(reason, 2048));
                        return ports::DeliveryResult::TerminalWaiting {
                            reason: if said.is_empty() {
                                "The member's terminal holds input".to_owned()
                            } else {
                                said
                            },
                            dialog: v["dialog"].as_str().map(str::to_owned),
                        };
                    }
                    // The project's own limits refuse it, by the answer's
                    // typed cause, never its sentence (zerus-63rru.23).
                    (Some("refused"), Some(reason))
                        if v["cause"] == crate::tasks::PROJECT_LIMIT =>
                    {
                        let said = super::cancel::reason::spoken(&cut(reason, 2048));
                        return ports::DeliveryResult::ProjectLimit {
                            reason: if said.is_empty() {
                                "The project's own limit refused the hand-out".to_owned()
                            } else {
                                said
                            },
                        };
                    }
                    // The task command reserved and sent nothing, for a cause
                    // that may pass (zerus-63rru.58): retry the same delivery.
                    (Some("not_started"), Some(reason)) => {
                        // Cleaned as the other delivery sentences are.
                        let said = super::cancel::reason::spoken(&cut(reason, 2048));
                        return ports::DeliveryResult::NotStarted {
                            reason: if said.is_empty() {
                                "The task writer did not start delivery".to_owned()
                            } else {
                                said
                            },
                        };
                    }
                    _ => {}
                }
            }
        }
    }
    delivery_reply(ended, id).into()
}
pub(super) fn delivery_reply(ended: Ended, id: &str) -> HandoutOutcome {
    match ended {
        Ended::Exited { code, output } => {
            if code != 0 {
                HandoutOutcome::Uncertain {
                    reason: "A failed child cannot confirm a delivery result".into(),
                }
            } else {
                decode_delivery(&output, id)
            }
        }
        Ended::NotStarted => HandoutOutcome::Refused {
            reason: "Delivery did not start".into(),
        },
        Ended::Unknown(_) => HandoutOutcome::Uncertain {
            reason: "The original delivery child did not settle".into(),
        },
    }
}
pub(super) use super::reply::decode_delivery;
impl Native {
    pub(super) fn done(
        &self,
        team: &Team,
        node: &Node,
        decision: &crate::teams::store::Decision,
        id: &str,
    ) -> Result<super::ports::Finish> {
        match self.command(team, node, None, id, "team-done", Some(decision))? {
            Ended::Exited { code: 0, output } => decode_finish(&output, &team.id, node.id),
            _ => Err(problem(
                "The original finishing child did not settle successfully",
            )),
        }
    }
}
pub(super) fn decode_finish(raw: &[u8], team: &str, node: u32) -> Result<super::ports::Finish> {
    use super::ports::Finish;
    let v: Value = serde_json::from_slice(raw).map_err(|_| problem("Malformed finishing reply"))?;
    if v["schema"] != 1 || v["team"] != team || v["node"].as_u64() != Some(u64::from(node)) {
        return Err(problem("Finishing reply does not echo this request"));
    }
    let reason = v["error"]
        .as_str()
        .ok_or_else(|| problem("Finishing reply has no error field"))?;
    Ok(match v["state"].as_str() {
        Some("done") => Finish::Done,
        Some("stale") => Finish::Stale,
        Some("refused") => Finish::Refused(cut(reason, 2048)),
        Some("cutting_over") => Finish::CuttingOver(cut(reason, 2048)),
        _ => return Err(problem("Unknown finishing reply state")),
    })
}

/// Shared with real-command contract tests; no adapter reconstructs this vector.
pub(super) fn occupancy_arguments(project: &str, session: &str) -> Vec<String> {
    [
        "task",
        "list",
        "--project",
        project,
        "--stage",
        "active",
        "--limit",
        "1",
        "--session",
        session,
        "--json",
    ]
    .map(String::from)
    .into()
}
pub(super) fn task_arguments(task: &TaskRef, action: &str) -> Vec<String> {
    vec![
        "task".into(),
        action.into(),
        format!("{}/{}", task.project, task.uuid),
        "--json".into(),
    ]
}

#[cfg(test)]
thread_local! {pub(super) static TEST_INVENTORY: std::cell::RefCell<Option<Value>> = const {std::cell::RefCell::new(None)};}

pub(super) fn task_request(
    team: &Team,
    node: &Node,
    member: Option<&Member>,
    id: &str,
) -> serde_json::Value {
    if let Some(m) = member {
        json!({"schema":1,"id":id,"team":team.id,"node":node.id,"member":m.id})
    } else {
        json!({"schema":1,"team":team.id,"node":node.id})
    }
}

#[cfg(test)]
thread_local! {
    pub(super) static CAPTURED_CHILD: std::cell::RefCell<Option<(Vec<String>, Value)>> = const {std::cell::RefCell::new(None)};
}
#[cfg(test)]
pub(super) fn capture_child(args: &[String], input: &Value) {
    CAPTURED_CHILD.with(|s| *s.borrow_mut() = Some((args.to_vec(), input.clone())));
}
