use crate::teams::{ops::HandoutOutcome, store::Store};
use zerus_team_plan::{model::*, Problem};
pub(super) type Result<T> = std::result::Result<T, Problem>;

pub(in crate::desktop::teams) trait Ports {
    fn clock_ms(&self, fallback: i64) -> i64 {
        fallback
    }

    fn cancels(
        &mut self,
        _store: &Store,
        _team: &str,
        _after: u32,
    ) -> Result<Vec<super::cancel::Pending>> {
        Ok(vec![])
    }
    fn cancel(
        &mut self,
        _team: &Team,
        _pending: &super::cancel::Pending,
        _original: &str,
    ) -> Result<super::cancel::Answer> {
        Err(unavailable())
    }
    fn record_cancel(
        &mut self,
        _store: &mut Store,
        _team: &Team,
        _fact: &super::cancel::Fact,
        _now: i64,
    ) -> Result<()> {
        Err(unavailable())
    }
    fn cancel_fact_outcome(
        &mut self,
        store: &mut Store,
        team: &Team,
        fact: &super::cancel::Fact,
        now: i64,
    ) -> Result<super::cancel::FactOutcome> {
        match self.record_cancel(store, team, fact, now) {
            Ok(()) => Ok(super::cancel::FactOutcome::Recorded),
            Err(error) => super::cancel::settle::classify(store, self, team, fact, error),
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn team_timers(
        &mut self,
        _state: &mut super::auxiliary::State,
        _store: &mut Store,
        _team: &Team,
        _now: i64,
    ) -> Result<()> {
        Ok(())
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn answer_timers(
        &mut self,
        _state: &mut super::auxiliary::State,
        _store: &mut Store,
        _team: &Team,
        _now: i64,
    ) -> Result<()> {
        Ok(())
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn terminal_timers(
        &mut self,
        _state: &mut super::auxiliary::State,
        _store: &mut Store,
        _now: i64,
    ) -> Result<()> {
        Ok(())
    }
    fn planned_task(
        &mut self,
        _team: &Team,
        _request: &crate::teams::store::publication::PlannedTaskRequest,
        _clear: bool,
    ) -> Result<TaskReply> {
        Err(unavailable())
    }
    fn stage(
        &mut self,
        _team: &Team,
        _effect: &crate::teams::store::StageTaskEffect,
        _original: &str,
    ) -> Result<TaskReply> {
        Err(unavailable())
    }
    fn clear_stage(
        &mut self,
        _team: &Team,
        _keyed: &crate::teams::store::timers::KeyedTask,
        _original: &str,
    ) -> Result<TaskReply> {
        Err(unavailable())
    }
    fn stopped(&self) -> bool {
        false
    }
    /// Journal hints only; this read must not decode any task document.
    fn changes(
        &mut self,
        project: &str,
        generation: &str,
        offset: u64,
    ) -> Result<zerus_replica::Changes>;
    /// The trusted adapter reads under the project lock and records the fact.
    fn observe(&mut self, store: &mut Store, task: &TaskRef) -> Result<()>;
    fn timing_supported(&self) -> bool {
        false
    }
    /// Default adapters retain their ordinary observation behavior. Only the
    /// native task-document reader returns a held, source-proven snapshot.
    fn observe_timing(
        &mut self,
        store: &mut Store,
        task: &TaskRef,
        _epoch: &str,
        _generation: Option<String>,
        _offset: Option<u64>,
    ) -> Result<Option<crate::teams::tasks::timing::TimingRead>> {
        self.observe(store, task)?;
        Ok(None)
    }
    /// Both a current native fleet row and the session's task index must agree.
    fn free(&mut self, team: &Team, member: &Member) -> Result<bool>;
    /// The same answer as [`Ports::free`] from the same read, and when the
    /// member cannot be read, why (zerus-63rru.55). An adapter that cannot
    /// tell keeps `free`'s answer: free or busy, never unreadable.
    fn availability(&mut self, team: &Team, member: &Member) -> Result<Availability> {
        Ok(if self.free(team, member)? {
            Availability::Free
        } else {
            Availability::Busy
        })
    }
    fn held_availability(
        &mut self,
        _store: &mut Store,
        team: &Team,
        member: &Member,
        _effect: &crate::teams::store::EffectIntent,
        _confirmation: Option<&crate::teams::ops::ProviderConfirmation>,
        _now: i64,
    ) -> Result<(Availability, Option<String>)> {
        Ok((self.availability(team, member)?, None))
    }
    /// A stamp of what the member's current fleet row says, by the values
    /// the actor reads (`fleet::stamp`), or None when it has no row now
    /// (zerus-63rru.79). A changed stamp is a changed row. An adapter that
    /// cannot tell has none, and a member's terminal fact then ends only
    /// with its time.
    fn row_stamp(&mut self, _team: &Team, _member: &Member) -> Result<Option<String>> {
        Ok(None)
    }
    fn report(&mut self, _task: &TaskRef) -> Result<Option<String>> {
        Ok(None)
    }
    fn reported(
        &mut self,
        _team: &Team,
        _member: &Member,
    ) -> Result<crate::teams::integrate::Reported> {
        Err(unavailable())
    }
    fn repository(&mut self, _team: &Team) -> Result<String> {
        Err(unavailable())
    }
    fn descends(&mut self, _plan: &MergePlan) -> Result<bool> {
        Err(unavailable())
    }
    fn contains(&mut self, _plan: &MergePlan) -> Result<Option<String>> {
        Err(unavailable())
    }
    fn merge(&mut self, _plan: &MergePlan) -> MergeResult {
        MergeResult::Unavailable {
            reason: unavailable().message,
        }
    }
    fn finish(
        &mut self,
        _team: &Team,
        _node: &Node,
        _decision: &crate::teams::store::Decision,
        _id: &str,
    ) -> Result<Finish> {
        Err(unavailable())
    }
    /// An uncertain original is never repeated. NotStarted proves no child
    /// ran and permits a later attempt with a fresh admission.
    fn deliver(&mut self, team: &Team, node: &Node, member: &Member, id: &str) -> DeliveryResult;
    /// Read the original hosted request's outcome. None asks for a later
    /// bounded read; it never authorizes reconstructing a current input.
    fn recover_delivery(&mut self, _team: &Team, _node: &Node, _member: &Member, _id: &str) -> Result<Option<HandoutOutcome>> {
        Ok(Some(HandoutOutcome::Uncertain { reason: "The service lost its original result; this delivery will not be sent again.".into() }))
    }
}

/// Whether a member can take a task now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::desktop::teams) enum Availability {
    Free,
    /// Known not to be free: working, waiting, stopped or occupied.
    Busy,
    /// Not free because the fact named here cannot be read; unknown is never
    /// free.
    Unreadable(Fact),
}

/// What cannot be read about a member: an unknown count of its fleet row
/// while it is otherwise free, or why its binding names no current run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::desktop::teams) enum Fact {
    /// A bound launch has not yet passed its native first-send gate.
    FirstMessage,
    /// `process_work.count`: the process table could not be verified.
    Background,
    /// `subagent_active_count`: no hook event has reported it yet.
    Children,
    /// `subagent_active_count` was reported, but the state of some subagent
    /// that may be running now cannot be settled (zerus-63rru.61).
    ChildrenUnsettled,
    /// `optional_questions.count`: the question index could not be read.
    Questions,
    /// Bound before session handles, with no source left that names it.
    NoHandle,
    /// No live or saved session carries its handle.
    Gone,
    /// Its durable archive has no matching catalog identity.
    Archive,
    /// Its session names another launch, agent or folder.
    Changed,
    /// The person started its session fresh.
    StartedFresh,
    /// The store, the record or the deadline did not answer.
    Unavailable,
    /// Its terminal refused a hand-out because it holds input: a question,
    /// a dialog or an unsent draft (zerus-63rru.79). The member can be read;
    /// its row says free, but its terminal is not. The dialog the gate
    /// knew, when one did (zerus-63rru.83).
    TerminalWaiting {
        dialog: Option<String>,
    },
}

/// Only a positive non-write permits another admission. NotStarted means
/// no original CLI child ran, or the task command answered that it reserved
/// and sent nothing for a cause that may pass; Stale is its explicit
/// non-write answer when the team's observation of the task is behind.
/// A lost result or possible submission is always Outcome(Uncertain).
#[derive(Clone, Debug)]
pub(in crate::desktop::teams) enum DeliveryResult {
    /// The task writer proved it wrote nothing because observation is behind.
    Stale,
    Outcome(HandoutOutcome),
    /// A hosted child may have offered the retained input but its answer
    /// was lost. Read the recorded original on a later pass; never rebuild.
    ReadLater,
    NotStarted {
        reason: String,
    },
    /// The task command refused before any input because the member's
    /// terminal holds input, by its answer's typed cause (zerus-63rru.79).
    /// Recorded as a refusal; a fact about the member, not a failed hand-out.
    TerminalWaiting {
        reason: String,
        /// Which dialog the member's terminal waits on, when the gate
        /// knows one (zerus-63rru.83).
        dialog: Option<String>,
    },
    /// The task command refused before any input because the project's own
    /// limits refuse it, by its answer's typed cause (zerus-63rru.23).
    /// Recorded as a refusal; a fact about the team, not a failed hand-out.
    ProjectLimit {
        reason: String,
    },
}
impl From<HandoutOutcome> for DeliveryResult {
    fn from(value: HandoutOutcome) -> Self {
        Self::Outcome(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(in crate::desktop::teams) enum TaskReply {
    Stale,
    Cancelled,
    Released,
    Created,
    Cleared,
    Refused { reason: String },
    CuttingOver { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::desktop::teams) enum Finish {
    Done,
    Stale,
    Refused(String),
    CuttingOver(String),
}

fn unavailable() -> Problem {
    Problem::new(
        zerus_team_plan::problem::code::INTEGRATION_UNAVAILABLE,
        "Team work reader is unavailable",
    )
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(in crate::desktop::teams) struct MergePlan {
    pub team: String,
    pub owner: String,
    pub machine: String,
    pub project: String,
    pub branch: String,
    pub base: String,
    pub repo: String,
    pub node: u32,
    pub generation: u32,
    pub decision: u64,
    pub member: String,
    pub commit: String,
    pub id: String,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(in crate::desktop::teams) enum MergeResult {
    Merged { commit: String },
    Conflict { files: Vec<String> },
    Unavailable { reason: String },
    Unknown { reason: String },
}
