use serde::{Deserialize, Serialize};
use zerus_team_plan::{catalog::TemplateBundle, model::*};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberReading {
    pub member: MemberId,
    pub session: SessionKey,
    pub unreadable: bool,
}

/// Construct only after the caller's native authority has been verified.
/// This type deliberately cannot be deserialized from an operation request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Actor {
    Person {
        person: String,
        origin: PersonOrigin,
    },
    Session {
        person: String,
        team: String,
        member: String,
        session: SessionKey,
    },
    Other {
        person: String,
        session: SessionKey,
    },
    /// Internal observer/recorder, never an authority available over the CLI.
    Service {
        person: String,
        team: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonOrigin {
    DesktopUi,
    CliInterim,
}

impl Actor {
    pub fn person(&self) -> &str {
        match self {
            Self::Person { person, .. }
            | Self::Session { person, .. }
            | Self::Other { person, .. }
            | Self::Service { person, .. } => person,
        }
    }
}

/// The task's team/home key as read by the trusted observer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyState {
    #[default]
    Ours,
    Absent,
    Other,
}
impl KeyState {
    fn is_ours(&self) -> bool {
        *self == Self::Ours
    }
}

/// A service note stored beside a member's report, never inside its text: a
/// short lower-case code and an optional bounded text. Readers show it under
/// their own heading and keep the member's words exactly as written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportNote {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<String>,
}

impl ReportNote {
    pub fn new(code: &str) -> Self {
        Self {
            code: code.to_owned(),
            text: None,
            parameters: Vec::new(),
        }
    }
    /// The English sentence a prompt or the command line shows under its own
    /// heading. An unknown code with text shows that text.
    pub fn sentence(&self) -> String {
        match self.code.as_str() {
            "text_shortened" => {
                "The service shortened the member's report text at its 15000-byte bound.".to_owned()
            }
            "worktree_dirty" => {
                "The member's worktree had uncommitted changes when it reported.".to_owned()
            }
            _ => self
                .text
                .clone()
                .unwrap_or_else(|| format!("Service note: {}", self.code)),
        }
    }
}

/// The task writer's authoritative snapshot, not an independently editable
/// node state. Revisions are the task document's opaque tokens. Each update
/// compares the previously projected token so delayed observations cannot
/// overwrite a newer projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskObservation {
    pub revision: String,
    #[serde(default, skip_serializing_if = "KeyState::is_ours")]
    pub key: KeyState,
    #[serde(default)]
    pub previous_revision: String,
    pub stage: TaskStage,
    #[serde(default)]
    pub cancelled: bool,
    /// The task's current attempt is in phase Cancelled: the cancel this
    /// team owns, read from the attempt pair after rule 1 cleared the mark
    /// (zerus-63rru.36). The node whose effect matches the attempt's ID
    /// ends on it; a newer node for the same task does not. Carried in
    /// memory beside the observation; never stored, so the stored JSON
    /// keeps the base field set (round 3).
    #[serde(default, skip)]
    pub current_cancelled: bool,
    pub rank: String,
    #[serde(default)]
    pub title: String,
    /// The highest generation any attempt of the task claims; 0 without one.
    #[serde(default)]
    pub attempt: u64,
    /// The ID of the task's current attempt; empty without one. An attempt
    /// the team hands out has its effect's request ID. At most 256 bytes.
    #[serde(default)]
    pub attempt_id: String,
    /// Two attempts claim the highest generation; the ID is then empty. An
    /// empty ID without this flag is an ordinary pair: no current attempt.
    #[serde(default)]
    pub attempt_conflict: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStage {
    Backlog,
    Todo,
    InProgress,
    InReview,
    Done,
}

/// A hand-out's or reopen's outcome. `HandedBack` is a final refusal handed
/// to the lead at once, never retried: a reopen whose conversation changed,
/// or a hosted hand-out refused as unsafe, canceled or dropped after claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum HandoutOutcome {
    Submitted { receipt: String },
    Refused { reason: String },
    CuttingOver { reason: String },
    Uncertain { reason: String },
    HandedBack { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum StageTaskOutcome {
    Created { task: TaskRef },
    Refused { reason: String },
    Uncertain { reason: String },
}

/// A service timer of package 10; each fires at most once per key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerKind {
    NoReport,
    NotStarted,
    RemindLead,
    RemindPerson,
    SilentLead,
}

/// The final answer of the team-clear child for one task key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyClearState {
    Cleared,
    Refused,
}

/// The service's record of a cancel decision it carried out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelState {
    /// The task was cancelled under that attempt. In a live team a bound
    /// task's end is recorded by its observation, so there this answer is
    /// refused; it ends a node without a task and settles after a stop.
    Cancelled,
    Refused,
    /// The key was already gone from the task: in every team state the
    /// answer is recorded and the node's binding goes with it.
    Released,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationProblemKind {
    Refused,
    Uncertain,
    Deferred,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    SetPhaseBarrier {
        scope: zerus_team_plan::phase_control::PhaseControlScope,
        barrier: bool,
    },
    PausePhase {
        scope: zerus_team_plan::phase_control::PhaseControlScope,
        pause: Option<String>,
        reason: String,
    },
    ResumePhase {
        scope: zerus_team_plan::phase_control::PhaseControlScope,
        pause: Option<String>,
    },
    MovePhase {
        scope: zerus_team_plan::phase_control::PhaseControlScope,
        relative_to: zerus_team_plan::phase_control::PhaseControlScope,
        placement: zerus_team_plan::phase_control::PhasePlacement,
    },
    SetPhaseAcceptance {
        scope: zerus_team_plan::phase_control::PhaseControlScope,
        acceptance: Acceptance,
    },
    RecordTimingFact {
        node: u32,
        observation: Box<super::observed_timing::TimingInput>,
    },
    SendBackPhase {
        phase: u32,
        publication: String,
        comment: String,
    },
    SubmitPhaseCorrections {
        phase: u32,
        publication: String,
        rework: String,
        draft: zerus_team_plan::draft::DraftInput,
    },
    EditPhaseCorrections {
        phase: u32,
        publication: String,
        rework: String,
        generation: u64,
        draft: zerus_team_plan::draft::DraftInput,
    },
    ApprovePhaseCorrections {
        phase: u32,
        publication: String,
        rework: String,
        generation: u64,
    },
    RejectPhaseCorrections {
        phase: u32,
        publication: String,
        rework: String,
        generation: u64,
        comment: String,
    },
    UndoPhaseCorrectionRejection {
        phase: u32,
        publication: String,
        rework: String,
        generation: u64,
        rejection: String,
    },
    ReconcileCorrectionReview {
        phase: u32,
        publication: String,
        rework: String,
        generation: u64,
    },
    ReconcilePhaseReview {
        phase: u32,
        generation: u64,
    },
    ApprovePhase {
        phase: u32,
        generation: u64,
        #[serde(default)]
        confirm_initial: bool,
    },
    RejectPhase {
        phase: u32,
        generation: u64,
        comment: String,
    },
    UndoRejectPhase {
        phase: u32,
        generation: u64,
        rejection: String,
    },
    ReconcilePublication {
        phase: u32,
        publication: String,
    },
    PublicationTaskProblem {
        publication: String,
        node: u32,
        reason: String,
        kind: PublicationProblemKind,
        #[serde(default)]
        clear: bool,
    },
    ReconcilePhase {
        phase: u32,
        publication: String,
    },
    AcceptPhase {
        phase: u32,
        publication: String,
    },
    SubmitDraftPhase {
        phase: u32,
        draft: zerus_team_plan::draft::DraftInput,
    },
    EditDraftPhase {
        phase: u32,
        generation: u64,
        draft: zerus_team_plan::draft::DraftInput,
    },
    StartPlanning {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_template: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delegations: Option<Delegations>,
        project: String,
        name: String,
        goal: String,
        home: String,
        bundle: Box<TemplateBundle>,
        integration_branch: String,
        base_commit: String,
    },
    Create {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_template: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delegations: Option<Delegations>,
        project: String,
        name: String,
        goal: String,
        home: String,
        bundle: Box<TemplateBundle>,
        integration_branch: String,
        base_commit: String,
    },
    AddMember {
        role: String,
        agent: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        effort: String,
        #[serde(default)]
        account: zerus_team_plan::catalog::AccountChoice,
    },
    LeadAddMember {
        role: String,
        agent: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        effort: String,
        #[serde(default)]
        account: zerus_team_plan::catalog::AccountChoice,
    },
    SwapMember {
        member: String,
        agent: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        effort: String,
        #[serde(default)]
        account: zerus_team_plan::catalog::AccountChoice,
    },
    RenameTeam {
        name: String,
    },
    SetGoal {
        goal: String,
    },
    SetMemberSession {
        member: String,
        session: SessionKey,
        account: String,
        worktree: String,
        ownership_nonce: String,
    },
    /// A launch-bound member's durable handle: the session ID of the record
    /// its binding verified (zerus-63rru.53). Written once, by the service.
    SetMemberHandle {
        member: String,
        session_id: String,
        #[serde(default)]
        label_hint: String,
    },
    SetMemberState {
        member: String,
        state: MemberState,
    },
    Pause,
    Resume,
    Stop,
    Archive,
    /// Person-only: advance the default for future hand-outs, without a Git write.
    AdvanceBase {
        commit: String,
    },
    PauseMember {
        member: String,
    },
    ResumeMember {
        member: String,
    },
    StopMember {
        member: String,
    },
    ArchiveMember {
        member: String,
    },
    AddTask {
        task: TaskRef,
        #[serde(default)]
        role: Option<String>,
        #[serde(default)]
        pipeline: Option<String>,
    },
    Admit {
        node: NodeId,
    },
    Route {
        node: NodeId,
        role: String,
        pipeline: String,
        #[serde(default)]
        deps: Vec<Dep>,
        #[serde(default)]
        writes: Vec<String>,
        #[serde(default)]
        assignee: Option<MemberId>,
    },
    /// The person edits dependencies before the first attempt or delivery
    /// reservation, without changing routing or assignment.
    SetDeps {
        node: NodeId,
        deps: Vec<Dep>,
    },
    Hold {
        node: NodeId,
    },
    Release {
        node: NodeId,
    },
    Cancel {
        node: NodeId,
        reason: String,
    },
    HandoutIntent {
        node: NodeId,
        member: String,
        effect_request_id: String,
    },
    HandoutOutcome {
        effect_request_id: String,
        outcome: HandoutOutcome,
    },
    ReopenIntent {
        node: NodeId,
        member: MemberId,
        effect_request_id: String,
    },
    ReopenOutcome {
        effect_request_id: String,
        outcome: HandoutOutcome,
    },
    ObserveTask {
        node: NodeId,
        observation: TaskObservation,
    },
    RecordReport {
        node: NodeId,
        generation: u32,
        member: MemberId,
        /// The member's own bounded text; a service note is never appended.
        text: String,
        commit: String,
        /// Service statements about the report, stored beside it.
        #[serde(default)]
        notes: Vec<ReportNote>,
    },
    StageTaskIntent {
        node: NodeId,
        effect_request_id: String,
    },
    StageTaskOutcome {
        effect_request_id: String,
        outcome: StageTaskOutcome,
    },
    Verdict {
        node: NodeId,
        generation: u32,
        pass: bool,
        text: String,
    },
    Accept {
        node: NodeId,
    },
    RecordIntegration {
        node: NodeId,
        decision: u64,
        commit: String,
    },
    RecordIntegrationConflict {
        node: NodeId,
        decision: u64,
        conflicts: Vec<String>,
    },
    Return {
        node: NodeId,
        reason: String,
    },
    Reassign {
        node: NodeId,
        member: String,
        reason: String,
    },
    Ask {
        node: NodeId,
        generation: u32,
        text: String,
    },
    Reply {
        item: u64,
        text: String,
    },
    OpenCard {
        kind: String,
        text: String,
        #[serde(default)]
        node: Option<NodeId>,
        /// The decisions the card takes: at most four short tokens of
        /// lower-case ASCII and underscores. Without it, acknowledged alone.
        /// Left out when absent, so that a request saved before the list
        /// existed replays byte for byte.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decisions: Option<Vec<String>>,
        /// The bounded parameters a localized window substitutes: at most 8
        /// keys of [a-z_], each value at most 256 characters after the
        /// sentence's rules (zerus-63rru.34). In memory only: a saved
        /// Request/OpenCard keeps the pre-change shape exactly, because the
        /// writer derives the values at write time from the ledger record it
        /// already carries. An older service reader must read every row a
        /// newer build writes.
        #[serde(default, skip_serializing)]
        parameters: Option<Vec<(String, String)>>,
    },
    CloseCard {
        card: u64,
        reason: String,
    },
    DecideCard {
        card: u64,
        decision: String,
    },
    SetValidationConfig {
        selection: zerus_team_plan::validation::Selection,
    },
    ChangeSettings {
        settings: Settings,
    },
    FireTimer {
        timer: TimerKind,
        node: NodeId,
        generation: u32,
        #[serde(default)]
        item: u64,
        #[serde(default)]
        since_ms: i64,
    },
    RecordKeyClear {
        node: NodeId,
        state: KeyClearState,
        #[serde(default)]
        reason: String,
    },
    RecordLateSession {
        member: MemberId,
        launch_id: String,
        session: SessionKey,
        name: String,
    },
    RecordCancel {
        node: NodeId,
        decision: u64,
        state: CancelState,
        #[serde(default)]
        reason: String,
        /// The task attempt the team-cancel child acted on: its generation
        /// and its ID (at most 256 bytes).
        #[serde(default)]
        attempt: u64,
        #[serde(default)]
        attempt_id: String,
    },
    /// A report that a removed member wrote after its removal took the task
    /// back (zerus-63rru.77): not recorded as the team's, told to the lead
    /// once per node and generation.
    RecordLateReport {
        node: NodeId,
        generation: u32,
        member: MemberId,
    },
    /// The service saw the kernel's unstaffed nodes differ from the lead's
    /// open items. The store recomputes the set itself, from its own view, and
    /// tells the lead of each new node once and closes the items of the nodes
    /// that left it. A stage with a permanent staffing gap is retired as at
    /// creation. A call that finds nothing to change is refused with
    /// `unstaffed_unchanged`.
    ReconcileUnstaffed,
    #[serde(skip_deserializing)]
    ReleaseProviderStop {
        node: u32,
        member: String,
        effect_request_id: String,
    },
    /// Record fresh availability readings for exact active member bindings.
    RecordMemberReadability {
        readings: Vec<MemberReading>,
    },
    /// Recompute the two aggregate waiting-queue inbox episodes. Service only.
    ReconcileLeadQueue,
    /// The service saw unended unstaffed items older than their node's
    /// newest one: stale items an earlier layout left, which stand for no
    /// episode. It ends at most a page of them, oldest first, in its own
    /// operation, before the reconcile judges the cap on the current
    /// episodes. A call that finds none is refused with
    /// `unstaffed_unchanged`.
    EndStaleUnstaffed,
    /// Close one page of refused-reopen notices that no longer call for lead action.
    CloseStaleReopenNotices,
    /// A hand-out reserved for a member that left the active set (paused,
    /// stopped, unbound) is settled at the next pass (zerus-63rru.85): the
    /// effect superseded, the node handed back to the lead with one item,
    /// and the next pass can hand the task to another free member. A member
    /// that is Active and bound is refused, whatever the reason says: the
    /// store's member state is the only proof of a departure.
    SettleLeftHandout {
        node: NodeId,
        /// Round 2's actor-read cause. A value here never authorizes a
        /// settlement and never names the item's cause: the store's member
        /// state decides both, and an Active bound member is refused. The
        /// field stays on the wire even when it is `None`: round 2 wrote
        /// `"reason":null`, and the strict Request decoder learns an
        /// operation's known fields by serializing it, so a dropped key
        /// would reject those saved requests as unknown fields
        /// (zerus-63rru.85 round 4).
        #[serde(default)]
        reason: Option<String>,
    },
    /// The service's own binding observation read the member's binding as
    /// decisively ended while the store still held it (zerus-63rru.85 round
    /// 3): the member is recorded unbound, its binding and durable handle
    /// cleared, so every later reader — the settlement above included —
    /// decides from the store. A member that is not Active and bound is
    /// refused with `invalid_state` and nothing changes.
    UnbindMember { member: MemberId },
    /// The lead is told once that a member out of the active set holds a
    /// started task (zerus-63rru.86): one member_left item per member and
    /// task; the task stays with the member. A call that finds no such
    /// task writes nothing.
    TellPausedStarted {
        member: MemberId,
        node: NodeId,
    },
    /// The member is back in the active set: its paused-started items
    /// close, so a later leave tells the lead again (zerus-63rru.86).
    ClosePausedStarted {
        member: MemberId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Request {
    pub request_id: String,
    pub team: String,
    pub expected_revision: u64,
    #[serde(flatten)]
    pub operation: Operation,
}

// Splitting the envelope explicitly keeps unknown operation fields strict;
// serde's flattened fields and deny_unknown_fields cannot be combined.
impl<'de> Deserialize<'de> for Request {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let mut fields = serde_json::Map::<String, serde_json::Value>::deserialize(d)?;
        let mut take = |key: &str| {
            fields.remove(key).ok_or_else(|| {
                D::Error::missing_field(match key {
                    "request_id" => "request_id",
                    "team" => "team",
                    _ => "expected_revision",
                })
            })
        };
        let request_id = serde_json::from_value(take("request_id")?).map_err(D::Error::custom)?;
        let team = serde_json::from_value(take("team")?).map_err(D::Error::custom)?;
        let expected_revision =
            serde_json::from_value(take("expected_revision")?).map_err(D::Error::custom)?;
        let operation: Operation =
            serde_json::from_value(serde_json::Value::Object(fields.clone()))
                .map_err(D::Error::custom)?;
        // Internally tagged unit variants otherwise ignore leftover fields.
        let known = serde_json::to_value(&operation).map_err(D::Error::custom)?;
        if let Some(unknown) = fields.keys().find(|key| known.get(*key).is_none()) {
            return Err(D::Error::custom(format!("unknown field `{unknown}`")));
        }
        Ok(Self {
            request_id,
            team,
            expected_revision,
            operation,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub ok: bool,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<u64>,
}
