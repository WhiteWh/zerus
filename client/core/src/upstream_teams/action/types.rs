//! The action types the desktop sends and the outcomes it gets: the wire
//! shapes, the confirmation a person is asked for, and the request identity
//! rule. The command is named by the action and the store names every refusal.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zerus_team_plan::model::{Dep, Integration, NodeId, Settings};

/// limit for every one of them.
pub const TEXT_CHARS: usize = 16_384;
/// The longest name an action sends.
pub const NAME_CHARS: usize = 200;
/// The longest stderr a failure keeps.
pub const DETAIL_CHARS: usize = 2000;

/// What the person is asked before a destructive action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Confirmation {
    /// The dialog's stable name, for the frontend's labels.
    pub name: String,
    pub title: String,
    pub text: String,
    /// The confirming button's text.
    pub action: String,
}

/// A create's permissions as the person confirmed them: every one of the
/// contract's five, each a boolean. Unlike the contract's `Delegations`, a
/// missing field is never filled in with its default, so a call that names
/// fewer cannot grant more than was shown; an unknown field is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateDelegations {
    pub accept_results: bool,
    pub answer_members: bool,
    pub confirm_trust: bool,
    pub resume_exited: bool,
    pub merge_accepted: bool,
}

/// One team action, as the webview asks for it (`{"kind": "...", ...}`).
/// Strict like the call around it: a field this version does not know is
/// refused, never left out of the request that is prepared.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TeamAction {
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
        acceptance: zerus_team_plan::Acceptance,
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
    ApprovePhase {
        phase: u32,
        generation: u64,
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
    AcceptPhase {
        phase: u32,
        publication: String,
    },
    StartPlanning {
        template: String,
        project: String,
        name: String,
        goal: String,
        home: String,
        integration_branch: String,
        base_commit: String,
        #[serde(default)]
        integration: Integration,
        expected_template: String,
        delegations: CreateDelegations,
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
    /// Creates a team from a catalog template; the CLI resolves the bundle,
    /// and `team` is the new team's id.
    Create {
        template: String,
        project: String,
        name: String,
        #[serde(default)]
        goal: String,
        /// The computer the lead runs on (its UUID).
        home: String,
        integration_branch: String,
        base_commit: String,
        /// The chosen template's integration mode: a branch is required only
        /// when it is a branch.
        #[serde(default)]
        integration: Integration,
        /// The digest of the template the form showed, as the catalog read
        /// gave it (opaque; never computed here). The command line refuses
        /// the create with `template_changed` when the template it resolves
        /// is no longer that one.
        expected_template: String,
        /// The five permissions exactly as the form shows them; they become
        /// the team's in place of the template's.
        delegations: CreateDelegations,
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
    /// Pauses a team: it hands nothing out until resumed.
    // A variant with no fields is written `{}` so that, like every other
    // variant, it refuses a field this version does not know.
    Pause {},
    /// Resumes a team the person paused.
    Resume {},
    /// Cancels every open task and hands nothing out again.
    Stop {},
    /// Hides a done or stopped team, keeping its history.
    Archive {},
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
    /// Person-reviewed advisory metadata, without execution policy.
    ValidationConfig {
        selection: zerus_team_plan::validation::Selection,
    },
    /// Changes the team's settings; the whole object, with its bounds.
    Settings {
        settings: Settings,
    },
    /// Answers an owner card: the person's decision.
    Decide {
        card: u64,
        decision: String,
    },
    /// Admits a waiting queue node.
    Admit {
        node: NodeId,
    },
    /// Routes a node to a role and a pipeline.
    Route {
        node: NodeId,
        role: String,
        pipeline: String,
        /// The window edits placement only. The native service binds the
        /// existing task fields when it prepares the ordinary Route request.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        keep_task_fields: bool,
        #[serde(default)]
        deps: Vec<Dep>,
        #[serde(default)]
        writes: Vec<String>,
        #[serde(default)]
        assignee: Option<String>,
    },
    /// Changes a task's dependencies before its first attempt or hand-out.
    SetDeps {
        node: NodeId,
        deps: Vec<Dep>,
    },
    /// Accepts a node's report.
    Accept {
        node: NodeId,
    },
    /// Returns a node to its implementer with the reason.
    Return {
        node: NodeId,
        reason: String,
    },
    /// Gives a node to another member.
    Reassign {
        node: NodeId,
        member: String,
        #[serde(default)]
        reason: String,
    },
    Hold {
        node: NodeId,
    },
    Release {
        node: NodeId,
    },
    Cancel {
        node: NodeId,
        #[serde(default)]
        reason: String,
    },
    /// Answers a lead inbox item (`hgs team reply`).
    Reply {
        item: u64,
        text: String,
    },
    /// A member's question to its lead (`hgs team ask`).
    Ask {
        node: NodeId,
        generation: u32,
        text: String,
    },
}

/// One call from the view: the team (the new id for Create), its name for the
/// confirmation text, the action, the revision the view was read at, whether
/// the person confirmed a destructive action, and the request id of the
/// intent, made once by the person's action.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamCall {
    #[serde(default)]
    pub team: String,
    #[serde(default)]
    pub team_name: String,
    pub action: TeamAction,
    #[serde(default)]
    pub expected_revision: Option<u64>,
    #[serde(default)]
    pub confirmed: bool,
    /// The intent's identity. The person's action makes it once, before the
    /// first send; the core never generates or replaces it.
    pub request_id: String,
}

/// How a call ended for the view.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TeamOutcome {
    /// hgs applied it; `receipt` is its answer.
    Done { receipt: Value, request_id: String },
    /// Nothing was applied: hgs refused it (its words and stable code) or the
    /// call never ran.
    Failed {
        detail: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
        request_id: String,
    },
    /// It may have been applied. Never sent again by the desktop by itself:
    /// read the team, then only the person may send it again with this
    /// `request_id`.
    Uncertain { detail: String, request_id: String },
    /// Not sent: the person confirms first.
    Confirm { confirmation: Confirmation },
    /// The same intent is still being sent; nothing new was sent. The
    /// desktop does nothing more for it.
    Pending { request_id: String },
}

impl TeamOutcome {
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Done { request_id, .. }
            | Self::Failed { request_id, .. }
            | Self::Uncertain { request_id, .. }
            | Self::Pending { request_id, .. } => Some(request_id),
            Self::Confirm { .. } => None,
        }
    }

    /// Whether the desktop may send the same call again by itself. Never for
    /// an uncertain one: its effect may have happened. Only the person repeats
    /// it, with the same request id.
    pub fn repeatable_by_desktop(&self) -> bool {
        matches!(self, Self::Failed { .. } | Self::Confirm { .. })
    }
}

/// A canonical UUID as the team store requires a request id: 8-4-4-4-12
/// lowercase hexadecimal digits.
pub fn canonical_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
}
