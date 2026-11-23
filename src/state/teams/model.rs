//! WhiteWh fork: the agent team record (docs/superpowers/specs/2026-10-24-agent-teams-core-design.md).
//! One team is stored whole as JSON; these types are its schema.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Task #7: a zero After-a-task threshold stays out of the stored JSON.
fn u32_is_zero(value: &u32) -> bool {
    *value == 0
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TeamStatus {
    /// F5: a team the owner and the lead are still drafting, before Planning.
    Draft,
    #[default]
    Planning,
    Running,
    Paused,
    Stopped,
    Done,
    Archived,
}

/// U59: how a team plans. Standard plans, approves, runs and closes each phase;
/// free keeps one open phase that takes tasks as they come until the owner
/// closes it. Only the owner switches, on a request bound to its id and content.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PlanMode {
    #[default]
    Standard,
    Free,
}

impl PlanMode {
    pub(crate) fn is_standard(&self) -> bool {
        *self == Self::Standard
    }
}

/// F5: a draft team's editing state: who changed what since the other side last
/// looked, so the owner's edit always wins and the lead sees the changes.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Draft {
    #[serde(default)]
    pub revision: u64,
    /// Fields the lead changed that the owner has not seen yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unseen: Vec<String>,
    /// Fields the owner edited after the lead's last read; a later lead edit of
    /// one is refused ("owner changed <field>").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_edited: Vec<String>,
    /// Task #13 (team ede724c4): the owner's settings the lead set in the
    /// draft (base_branch, branch_per_task, max_members, ...), by field: the
    /// owner's values stay, the lead's are offers New team marks and applies
    /// with one click. An offer goes when the owner applies it or sets the
    /// field otherwise.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub setting_offers: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FolderPolicy {
    /// One worktree per writing member.
    Worktree,
    /// Every member works in the team's folder.
    Shared,
    /// The lead chooses per member while planning.
    #[default]
    Lead,
    /// The lead chooses per member: shared, a worktree or a clone (B6).
    Auto,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Approval {
    /// The plan waits for Approve & Run.
    #[default]
    Review,
    /// The plan runs as soon as it has no errors.
    Auto,
}

/// U62: who may start a full test run (the whole suite, never a member's
/// affected tests): never; ask, meaning the owner's one-time grant for a
/// release; or allowed.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FullTests {
    Never,
    #[default]
    Ask,
    Allowed,
}

/// U62: the owner's permission for one full run, bound to a release.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct FullTestsGrant {
    pub release: String,
    pub at: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
}

/// Task 126 (owner, 2026-11-07): a team's memory. `plain` is memory as
/// before; `lore` is RustCodeLore: a shared workspace in the project's store
/// (`source` "project") or the team's own store under the state folder
/// (`source` "team"). New team names only the mode; Zerus fills the rest.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct TeamMemory {
    pub mode: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub store: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub workspace: String,
}

/// Independent owner opt-ins. Older teams keep both checks off.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PhaseChecks {
    #[serde(default)]
    pub critical_path_target: bool,
    #[serde(default)]
    pub unresolved_prerequisites: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(untagged, deny_unknown_fields)]
pub(crate) enum Prerequisite {
    Task { task: u32 },
    Commit { commit: String },
    Branch { branch: String },
}

/// Saved only on mutations, never computed by a GUI refresh.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct PhaseCheckResult {
    pub known_hours: Option<f64>,
    pub missing_estimates: usize,
    pub target_exceeded: bool,
    pub unresolved: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct Settings {
    #[serde(default)]
    pub phase_checks: PhaseChecks,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iron_rules: Vec<String>,
    /// Task 126: the team's memory; None is plain memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<TeamMemory>,
    /// Every dependency is hard, whatever the lead chose.
    #[serde(default)]
    pub all_hard: bool,
    #[serde(default)]
    pub folder_policy: FolderPolicy,
    #[serde(default)]
    pub approval: Approval,
    /// How new phases are approved; absent means the plan's `approval`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_approval: Option<Approval>,
    #[serde(default = "default_max_members")]
    pub max_members: usize,
    /// Idle time after a delivered turn ends before a member shows No report.
    #[serde(default = "default_report_timeout")]
    pub report_timeout_seconds: u64,
    /// How long a delivered hand-off may stay unstarted before the lead is
    /// told once that the member has not started (A1).
    #[serde(default = "default_start_timeout")]
    pub start_timeout_seconds: u64,
    /// How long a finished turn may stay blocked by an open question or by
    /// Background work before the lead is told once (N5).
    #[serde(default = "default_blocked_timeout")]
    pub blocked_timeout_seconds: u64,
    /// At most one inbox message reaches the lead per this many seconds (C2).
    #[serde(default = "default_inbox_interval")]
    pub inbox_interval_seconds: u64,
    /// Owner, 2026-11-17: minutes after the lead's last inbox message before
    /// a free lead whose inbox still holds open items, none of them new, is
    /// reminded to read it. Each reminder without a new item doubles the
    /// wait, up to an hour; a new list starts over. 0 turns it off.
    #[serde(default = "default_inbox_reminder")]
    pub inbox_reminder_minutes: u64,
    /// Task 186 (docs/lore.md 11.11 `urgent_bound`): minutes an urgent team
    /// message may wait for a safe point (or, rung once, for its read) before
    /// the lead hears, or the owner for a message to the lead; 1 to 120.
    #[serde(default = "default_urgent_bound")]
    pub urgent_bound_minutes: u64,
    /// Task 277 (owner, 2026-11-08): minutes an urgent message may stay
    /// unread before it is an alarm in the lead's inbox (and, for the owner's
    /// own message, on the owner's Waiting list); 0 turns the alarm off.
    #[serde(default = "default_urgent_alarm")]
    pub urgent_alarm_minutes: u64,
    /// Minutes an undecided report waits before the lead is reminded once;
    /// 0 turns the reminder off (C4).
    #[serde(default = "default_remind_lead")]
    pub remind_lead_minutes: u64,
    /// Minutes before an undecided report shows on the owner's attention list in
    /// the desktop; 0 turns it off (C4).
    #[serde(default = "default_remind_owner")]
    pub remind_owner_minutes: u64,
    /// U18: minutes a member's unanswered permission request waits before it
    /// reaches the owner's attention list; 0 turns only that off.
    #[serde(default = "default_remind_owner_permission")]
    pub remind_owner_permission_minutes: u64,
    /// Task #286 (owner, 2026-11-08): minutes a task's wait for the lead (or
    /// the merger) may remain unresolved before it reaches the owner as one
    /// escalation item naming the lead; 0 never escalates. A merge-conflict
    /// wait never reaches the owner: it is the lead's or the merger's.
    #[serde(default = "default_owner_escalate")]
    pub owner_escalate_minutes: u64,
    /// Task 95 (owner, 2026-11-06): minutes the lead's decisions (reports,
    /// questions, permissions) may wait while its session does not work on
    /// them before the owner is told the lead stalled; 0 turns it off. Only
    /// the owner sets it.
    #[serde(default = "default_lead_stall")]
    pub lead_stall_minutes: u64,
    /// Task 97 (owner, 2026-11-06): minutes a native permission prompt or
    /// question of the lead or a member may wait unanswered before Zerus
    /// answers it with the safe choice (Deny; the recommended or first option)
    /// and queues it for the owner; 0 turns it off. Only the owner sets it.
    #[serde(default = "default_defer")]
    pub defer_minutes: u64,
    /// Minutes a Background job without a declared budget may run before the
    /// lead hears it is red (C12).
    #[serde(default = "default_background_red")]
    pub background_red_minutes: u64,
    /// A project that forbids worktrees and clones (B6): members then work in
    /// the shared or their own existing folder only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub worktrees_forbidden: bool,
    /// H1: resume a member's session after a usage-limit reset, even when the
    /// shared automatic-recovery policy is off.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resume_after_limits: bool,
    /// Task 209 (owner, 2026-11-07): the native context pressure, in percent of
    /// the agent's window, at which Zerus compacts a member before it continues.
    /// 95 by default; the owner sets it.
    #[serde(default = "default_context_compact_percent")]
    pub context_compact_percent: u8,
    /// Task 254 (owner, 2026-11-08): a session whose prompt cache is cold and
    /// whose context holds at least this share of its window is compacted
    /// before a hand-out or a crash give-back; 30 by default, 0 never. The
    /// owner sets it.
    #[serde(default = "default_cold_compact_percent")]
    pub cold_compact_percent: u8,
    /// Task #228 (owner, 2026-11-07): the folder a QA member's scratch teams
    /// start in; empty is <team folder>/.qa-scratch. Only the owner sets it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scratch_root: String,
    /// N19 (spec 7, 12): folders whose images and HTML the desktop previews
    /// as report evidence, besides the team and member folders.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_folders: Vec<String>,
    /// Hierarchy (section 9.2), set on the master team: how deep sub-teams may
    /// go below it and how many sessions the whole tree may run.
    #[serde(default = "default_tree_depth", skip_serializing_if = "tree_depth_is_default")]
    pub tree_depth: u32,
    #[serde(default = "default_tree_sessions", skip_serializing_if = "tree_sessions_is_default")]
    pub tree_sessions: u32,
    /// Who approves a sub-team's plan and phases (section 9.3).
    #[serde(default, skip_serializing_if = "subteam_approval_is_default")]
    pub subteam_approval: SubteamApproval,
    /// U25: draft phases without errors are accepted at once, also the ones
    /// waiting when the owner turns it on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_accept: bool,
    /// U48: the owner allows a pipeline's land stage, which may push; without
    /// it a land stage holds the task for the lead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub land_allowed: bool,
    /// U48: each hand-out starts the task's own branch in the member's folder;
    /// absent keeps one branch per member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_per_task: Option<BranchPerTask>,
    /// U48: the branch a task's branch starts from; empty is the lead's branch.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base_branch: String,
    /// Task #19 (team ede724c4, owner 2026-11-10): the line the lead lands
    /// accepted work on by hand (manual landing): the merge queue is off for
    /// the team (an accept queues nothing and runs no suites), `hgs team
    /// landed` takes a commit on this branch of the team folder's repository,
    /// members merge it (git-merge-lead) and hand-outs start from it. Empty:
    /// the queue or the lead's branch as before. The lead may set it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub manual_line: String,
    /// Task 174 (owner, 2026-11-07): the checkout of the team's repository
    /// whose branch integrates the team's work: the merge queue merges into
    /// it and tests there, and members merge from it (git-merge-lead). Empty
    /// is the team's folder. Only the owner sets it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub integration_folder: String,
    /// U52: a member session that exits on its own is launched again into its
    /// conversation, at most three times an hour. On by default (owner,
    /// 2026-11-05), also for a team stored before, and always written so the
    /// desktop reads an explicit off.
    #[serde(default = "default_restart_crashed")]
    pub restart_crashed: bool,
    /// U56: minutes a busy member may go without a session event before Zerus
    /// marks it stuck; 0 (unset) reads the default of 10.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub stuck_minutes: u32,
    /// U56: the lead may interrupt or relaunch a stuck member; the owner sets it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lead_restarts_stuck: bool,
    /// Task 24: a member still working on an attempt that a reassign, a
    /// take-over or a cancel revoked has its turn interrupted once, before its
    /// stop notice; off by default, only the owner sets it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interrupt_revoked: bool,
    /// U63 (owner, 2026-10-31): the lead removes its own phase pause by
    /// itself; off by default, when it needs the owner's word.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lead_removes_own_pause: bool,
    /// U63: who accepts a new phase; the lead by default.
    #[serde(default, skip_serializing_if = "PhaseAcceptance::is_lead")]
    pub phase_acceptance: PhaseAcceptance,
    /// #752: members sharing one folder build their own snapshots through the
    /// build broker (broker.rs); off by default, the owner sets it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub build_broker: bool,
    /// U56: every task with a planned assignee stays with it, as a pinned
    /// task does; off by default, so a free same-role member may take it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_assignees: bool,
    /// U62: the owner's override of the project's `.zerus/tests.toml`, merged
    /// over it field by field (providers by name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests: Option<serde_json::Value>,
    /// U62: who may start the full test run; the owner's one-time grant is
    /// `Team.full_tests_grant`.
    #[serde(default, skip_serializing_if = "full_tests_ask")]
    pub full_tests: FullTests,
}

/// The default policy is not written into the team record.
fn full_tests_ask(value: &FullTests) -> bool {
    *value == FullTests::Ask
}

/// U48: the name of a task's branch: `{id}`, `{member}` and `{team8}` are
/// filled in, and the result is a valid Git branch name.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct BranchPerTask {
    pub template: String,
}

fn default_restart_crashed() -> bool {
    true
}

fn default_max_members() -> usize {
    6
}
/// Section 9.2: two levels of sub-teams below the master, and sixteen active
/// sessions in the whole tree.
fn default_tree_depth() -> u32 {
    2
}
fn default_tree_sessions() -> u32 {
    16
}
fn tree_depth_is_default(depth: &u32) -> bool {
    *depth == default_tree_depth()
}
fn tree_sessions_is_default(sessions: &u32) -> bool {
    *sessions == default_tree_sessions()
}
fn subteam_approval_is_default(approval: &SubteamApproval) -> bool {
    *approval == SubteamApproval::Parent
}
fn default_report_timeout() -> u64 {
    120
}
fn default_start_timeout() -> u64 {
    180
}
fn default_blocked_timeout() -> u64 {
    1800
}
fn default_inbox_interval() -> u64 {
    120
}
fn default_inbox_reminder() -> u64 {
    10
}

fn default_urgent_bound() -> u64 {
    10
}
/// Task 277: an urgent message unread this long is an alarm; 3 minutes.
fn default_urgent_alarm() -> u64 {
    3
}
fn default_remind_lead() -> u64 {
    15
}
fn default_remind_owner() -> u64 {
    60
}
/// U18: a permission request reaches the owner sooner than an undecided report:
/// the member is blocked meanwhile.
fn default_remind_owner_permission() -> u64 {
    10
}
/// Task #286: the owner hears of an unresolve lead wait after 60 minutes.
pub(crate) const DEFAULT_OWNER_ESCALATE_MINUTES: u64 = 60;
fn default_owner_escalate() -> u64 {
    DEFAULT_OWNER_ESCALATE_MINUTES
}
/// Task 95: the owner hears of a stalled lead after 20 minutes.
pub(crate) const DEFAULT_LEAD_STALL_MINUTES: u64 = 20;
fn default_lead_stall() -> u64 {
    DEFAULT_LEAD_STALL_MINUTES
}
/// Task 97: a team session's prompt waits 10 minutes for an answer.
pub(crate) const DEFAULT_DEFER_MINUTES: u64 = 10;
fn default_defer() -> u64 {
    DEFAULT_DEFER_MINUTES
}
fn default_background_red() -> u64 {
    30
}
/// Task 209: a member's context is compacted at 95% of its native window.
pub(crate) const DEFAULT_CONTEXT_COMPACT_PERCENT: u8 = 95;
pub(crate) const DEFAULT_COLD_COMPACT_PERCENT: u8 = 30;
fn default_cold_compact_percent() -> u8 {
    DEFAULT_COLD_COMPACT_PERCENT
}
fn default_context_compact_percent() -> u8 {
    DEFAULT_CONTEXT_COMPACT_PERCENT
}
fn default_phase() -> u32 {
    1
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            phase_checks: PhaseChecks::default(),
            iron_rules: Vec::new(),
            memory: None,
            all_hard: false,
            folder_policy: FolderPolicy::default(),
            approval: Approval::default(),
            phase_approval: None,
            max_members: default_max_members(),
            report_timeout_seconds: default_report_timeout(),
            start_timeout_seconds: default_start_timeout(),
            blocked_timeout_seconds: default_blocked_timeout(),
            inbox_interval_seconds: default_inbox_interval(),
            inbox_reminder_minutes: default_inbox_reminder(),
            urgent_bound_minutes: default_urgent_bound(),
            urgent_alarm_minutes: default_urgent_alarm(),
            remind_lead_minutes: default_remind_lead(),
            remind_owner_minutes: default_remind_owner(),
            remind_owner_permission_minutes: default_remind_owner_permission(),
            owner_escalate_minutes: default_owner_escalate(),
            lead_stall_minutes: default_lead_stall(),
            defer_minutes: default_defer(),
            background_red_minutes: default_background_red(),
            worktrees_forbidden: false,
            resume_after_limits: false,
            context_compact_percent: DEFAULT_CONTEXT_COMPACT_PERCENT,
            cold_compact_percent: DEFAULT_COLD_COMPACT_PERCENT,
            scratch_root: String::new(),
            evidence_folders: Vec::new(),
            tree_depth: default_tree_depth(),
            tree_sessions: default_tree_sessions(),
            subteam_approval: SubteamApproval::default(),
            auto_accept: false,
            land_allowed: false,
            branch_per_task: None,
            base_branch: String::new(),
            manual_line: String::new(),
            integration_folder: String::new(),
            restart_crashed: true,
            stuck_minutes: 0,
            lead_restarts_stuck: false,
            interrupt_revoked: false,
            lead_removes_own_pause: false,
            phase_acceptance: PhaseAcceptance::Lead,
            build_broker: false,
            keep_assignees: false,
            full_tests: FullTests::default(),
            tests: None,
        }
    }
}

/// A member's Zerus session, once it exists.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct SessionRef {
    /// SSH alias of the member's computer; empty for this computer.
    #[serde(default)]
    pub host: String,
    pub name: String,
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub conversation_id: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemberState {
    /// Planned; its session starts on approval.
    #[default]
    NotCreated,
    Active,
    Paused,
    Removed,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemberFolder {
    Worktree,
    /// A full clone with its own `.git`, so commit, merge and fetch work
    /// inside a sandboxed member's folder (B4/B6).
    Clone,
    /// Task #26: the catalog's `read_only` folder (Architect, Reviewer,
    /// Security, Researcher) is the shared folder with no own checkout, as
    /// the desktop already reads it; a draft or plan member may name it.
    #[default]
    #[serde(alias = "read_only")]
    Shared,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct PromptSnapshot {
    #[serde(default)]
    pub prompt_order: Vec<String>,
    #[serde(default)]
    pub prompt_off: Vec<String>,
    #[serde(default)]
    pub protocol_extra: String,
    #[serde(default)]
    pub task_template: String,
    #[serde(default)]
    pub notes: std::collections::BTreeMap<String, String>,
    /// U46: the role's `context_before_task` when the member joined, so a later
    /// catalog edit changes no running team.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub context_before_task: String,
}

/// Task 134: a member's unread team messages, kept in the team record so
/// status, the lead's inbox and the dispatcher's second ring read it without
/// the working tier. `oldest` is the id of the oldest unread message.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Unread {
    pub count: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub urgent: u32,
    pub oldest: String,
    pub oldest_at: f64,
    /// Task 277 (owner, 2026-11-08): the oldest unread urgent message, its
    /// age and who sent it, so the lead's alarm and the owner's Waiting list
    /// can name it without reading the working tier.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub urgent_oldest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgent_at: Option<f64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub urgent_by: String,
    /// How often the doorbell rang for the oldest one.
    #[serde(default)]
    pub rings: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rung_at: Option<f64>,
    /// The lead was told about the oldest one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lead_told: bool,
    /// Task 186 (11.11): the schedule below is each message's own, not the
    /// oldest's; a record written before it has none and rings as before.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub planned: bool,
    /// When the next doorbell is due: a message that never rang (an urgent
    /// one at once, a normal one at most every 5 minutes), or one that rang
    /// once and waited past its bound. None: nothing to ring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring_at: Option<f64>,
    /// When the lead hears that the member does not read: a message rang
    /// twice and waited past its bound again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_at: Option<f64>,
    /// The oldest urgent message that has reached no safe point yet (never
    /// rung, never put into a turn): past `urgent_bound` the lead hears, or
    /// for a message to the lead the owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_urgent_at: Option<f64>,
}

/// Task 277 (owner, 2026-11-08): an urgent message interrupted (or tried to
/// interrupt) the member's busy turn. After the native stop the message opens
/// a new turn that continues the same attempt; the attempt itself is kept.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct UrgentInterrupt {
    /// The unread urgent message ids the interrupt was requested for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
    /// The oldest unread urgent message at the request, so a newer urgent
    /// message may still interrupt after a failure.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub oldest: String,
    #[serde(default)]
    pub at: f64,
    /// The turn the interrupt was pinned to (the session's `turn_started`).
    #[serde(default)]
    pub turn: f64,
    /// requested, sent or failed.
    #[serde(default)]
    pub state: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    /// Owner, 2026-11-15: how often the continuation after the stop was sent
    /// again because the member never started a new turn.
    #[serde(default, skip_serializing_if = "no_retries")]
    pub retries: u32,
}

fn no_retries(value: &u32) -> bool {
    *value == 0
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Member {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iron_rules: Vec<String>,
    /// Stable within the team, such as `backend`.
    pub id: String,
    #[serde(default)]
    pub role: String,
    /// N3: the role's prompt blocks copied when the member joined, so a later
    /// catalog edit never changes a running team mid-phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_snapshot: Option<PromptSnapshot>,
    /// U46: compact or clear the agent's context before each hand-out. Empty
    /// inherits the role's setting; "none" overrides it with none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub context_before_task: String,
    /// Task #7: what a free member does with its context after a task, before
    /// its next one: empty keeps today's behaviour (U46 only), "compact" or
    /// "clear" runs the action only when the criterion below says the context
    /// is spent. Applies to the lead as to any member.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub after_task: String,
    /// Task #7: the criterion the policy checks: "percent" of the context
    /// window, or "tokens" used.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub after_task_on: String,
    /// Task #7: the threshold for that criterion: 1-100 percent, or a token
    /// count; zero when the member keeps its context.
    #[serde(default, skip_serializing_if = "u32_is_zero")]
    pub after_task_at: u32,
    /// U46: the member already got the feed note that its agent has no native
    /// context action; it is written once per member.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub context_noted: bool,
    /// Task 209: the last native context reading of the member's session: the
    /// used tokens, the window and their ratio, as its agent reports them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<MemberContext>,
    /// Task 209: a compaction, or a fresh conversation, Zerus is waiting to
    /// confirm before the member continues; the request survives a worker
    /// restart, so the action is never run twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_compact: Option<MemberCompact>,
    /// Task 209: a compaction that refused or stayed unconfirmed is retried no
    /// sooner than this moment, so a broken agent is not asked every tick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_retry_at: Option<f64>,
    pub agent: String,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub effort: String,
    /// The launch permission mode (C3): auto or bypass; empty for the agent's
    /// default.
    #[serde(default)]
    pub permissions: String,
    /// The role's permissions profile (B5): read, write, build, commit or
    /// network, mapped to the agent's own mode where it has one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub permissions_profile: String,
    /// The lead's or owner's answers to this member's permission requests,
    /// newest last, at most 20 (B1): {question_id, decision, by, at, reason}.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permission_answers: Vec<serde_json::Value>,
    /// SSH alias of the computer; empty for the lead's computer.
    #[serde(default)]
    pub computer: String,
    #[serde(default)]
    pub folder: MemberFolder,
    /// The folder on the member's computer; empty for the team's folder.
    #[serde(default)]
    pub directory: String,
    #[serde(default)]
    pub session: Option<SessionRef>,
    /// U32: how the member joined on the owner's verified word:
    /// `{by, owner_word, at}`; absent when the owner added it directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added: Option<Value>,
    #[serde(default)]
    pub state: MemberState,
    /// U49: the member finishes its open attempt and stage run, takes nothing
    /// new, then its session is paused and the member leaves the roster. Absent
    /// on every team stored before this field, which keeps today's behaviour.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub draining: bool,
    /// Recorded before the dispatcher starts the member's session, which
    /// `hgs ls` then shows under the same id.
    #[serde(default)]
    pub launch_id: String,
    /// Why the session could not start; the dispatcher does not try again
    /// until you relaunch it.
    #[serde(default)]
    pub error: String,
    /// Task 22: starts that failed only because the DeepSeek host was still
    /// starting. The dispatcher starts the member again by itself, at most
    /// `HOST_WAITS` times, before it records the error; a start or a
    /// relaunch clears it.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub host_waits: u32,
    /// Task #17 (team ede724c4): the start after a swap or relaunch waits
    /// because the session it replaces still works in the member's folder.
    /// The old session keeps the member's name and its open attempt until its
    /// turn ends and it is paused; only then does the new one start. Set
    /// while waiting, so the wait is said once.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replaced_busy: bool,
    /// Owner, 2026-11-11 (team 86821caa): since when the start waits for the
    /// replaced session. The wait was one feed event and nothing else: the
    /// member read `not_created` with no reason, and neither the lead nor the
    /// owner was told while its task stood for hours. The lead's inbox and
    /// Waiting for you name it from this moment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_since: Option<f64>,
    /// The role's instructions from the catalog, given with every task.
    #[serde(default)]
    pub prompt: String,
    /// Task #6: the start prompt changed while the member ran. The dispatcher
    /// sends it as the message that replaces its earlier instructions and
    /// clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_change: Option<PromptChange>,
    /// Task #6: the model and effort its running session must still switch to.
    /// The dispatcher switches before the member's next task and clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_switch: Option<ModelSwitch>,
    /// DeepSeek takes its model and effort after it starts; once tried, the
    /// dispatcher does not try again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub settings_applied: bool,
    /// U45: the models this member may switch between before a task; empty
    /// means only its launch `model` and `effort`. Copied from the catalog
    /// role when it joins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelChoice>,
    /// The member's questions already given to the lead, as question id and
    /// hash; the most recent ones only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asked: Vec<String>,
    /// The permission request the lead's inbox names while it waits for an
    /// answer (C2): the member's session question as Zerus read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<PendingQuestion>,
    /// A request of the member's session that Zerus cannot answer (Kimi takes
    /// its Edit and Write approvals only in its Terminal panel): neither the
    /// lead nor a safe action can, so the lead hears of it once and the owner
    /// sees it at once, with where to answer it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_question: Option<PendingQuestion>,
    /// The first tick that saw this member idle without a ready task while
    /// tasks were open (C7); cleared when it gets work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starved_since: Option<f64>,
    /// The lead was told once about this starvation episode (C7).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub starved_told: bool,
    /// Task 173: the member's session works with nothing handed to it
    /// (unassigned work), since when and what it runs; cleared once it has a
    /// hand-out or stops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unassigned: Option<Unassigned>,
    /// Task 134: the member's unread team messages (the working tier), as
    /// status shows them ("unread: N"); None when it has read everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread: Option<Unread>,
    /// Task 277: an urgent message that interrupted (or tried to interrupt)
    /// this member's busy turn; cleared once the new turn carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgent: Option<UrgentInterrupt>,
    /// Task 98: the member's Terminal refuses hand-outs because its composer
    /// holds a draft, a dialog or a prompt Zerus does not know; cleared by the
    /// next delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_block: Option<InputBlock>,
    /// Task 113: the request only its Terminal takes (`terminal_question`)
    /// waited past the owner queue's `defer_minutes`: the owner's item and the
    /// phone alert carry the screen's last lines. It belongs to that request
    /// only and goes with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_escalation: Option<TerminalEscalation>,
    /// Task 139: the last cross-check of its sources (look.rs); `uncertain`
    /// when they disagreed past the grace. Cleared when it is no longer suspect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<MemberCheck>,
    /// Task 97: the questions (`id:hash`) Zerus answered with the safe choice
    /// and queued for the owner, newest last, at most 50. A question here is
    /// never answered by Zerus again, whatever its delivery was.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred: Vec<String>,
    /// The member's declared time budget for its background work (C12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<Budget>,
    /// The first tick that saw Background work in this member's session; the
    /// default red time counts from here (C12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_since: Option<f64>,
    /// The lead heard once that this Background work passed the default red
    /// time (C12).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub background_told: bool,
    /// The MCP servers and services the member's session runs: they live with
    /// the session and are not Background work (G7).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<MemberService>,
    /// Task 278: what the member waits on now: the build Zerus runs for it, a
    /// background job its hooks registered, Background work under a budget.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<MemberJob>,
    /// The conversation whose notifications the dispatcher muted: a member
    /// works for the lead, so its replies do not call the owner. Muted once
    /// per conversation; the owner may unmute it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub quieted: String,
    /// The last liveness the dispatcher saw for this member's session: status
    /// derives `stopped` from it without a listing of its own (L3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen: Option<MemberSeen>,
    /// U56: the member's session phase and activity as the dispatcher last saw
    /// them, and when it last reported an event: status shows them without a
    /// listing of its own.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub seen_phase: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub seen_activity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_event_at: Option<f64>,
    /// Task 254: its session's prompt cache was cold at the last listing
    /// (`session_state` cold, or its cache TTL passed since it was last active).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub seen_cold: bool,
    /// U56: a busy member with no session event for stuck_minutes, with the
    /// reason and the phase it was in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck: Option<MemberStuck>,
    /// #761: a DeepSeek member with no open attempt is read before it is
    /// called stuck (its listing keeps "working" after the turn ends): when the
    /// worker last read it, and the newest native event that read found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck_read_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_at: Option<f64>,
    /// #728: the lead or the owner asked to interrupt and resume a stuck
    /// member; the dispatcher runs the session commands and clears this.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restart_pending: bool,
    /// Task 154: its rights changed while its session runs; the dispatcher
    /// pauses the session and resumes it in its conversation with them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rights_relaunch: bool,
    /// The permission request the lead last heard about (B1): the exact
    /// command, where it came from and the agent's reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<serde_json::Value>,
    /// H1: the member's session is stopped by a usage limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limited: Option<MemberLimit>,
    /// DCB-svn report (2026-11-12): the member's live session ended its turn
    /// on a provider error other than a usage limit; the lead hears it at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_failed: Option<TurnFailure>,
    /// When Zerus asked the member to continue after a temporary provider
    /// error, within the last hour (at most FAILURE_CONTINUES_PER_HOUR).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failure_continues: Vec<f64>,
    /// H1: Zerus asked the recovery service to wait for this member's reset.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub limit_resume: bool,
    /// N29: the session that request names, so a swap or the end of the
    /// episode withdraws it for that session, not for the next one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_session: Option<SessionRef>,
    /// N30: when this member's remote reset request was last refreshed. A
    /// refresh on another computer costs an `hgs` call with a 60 s bound, so a
    /// slow peer is asked once a minute, never on every tick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_refreshed_at: Option<f64>,
    /// H6: a swapped member's new session starts even before its next task.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub launch_after_swap: bool,
    /// Task 135: the fresh session of a swap has not had its handoff brief yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<HandoffNeed>,
    /// Owner feedback 2026-11-04 (14): the session a swap replaced, which the
    /// dispatcher pauses and renames before the new session takes the same
    /// name; None once it is set aside or gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced: Option<SessionRef>,
    /// Spec 5.6 (U25): the member it reports to - the lead (empty) or a
    /// sub-lead of the same team.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent: String,
    /// Spec 5.6: a sub-lead hands out, reviews and accepts the tasks of the
    /// members that report to it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sub: bool,
    /// A sub-lead's own coalesced inbox, as the team's is the lead's (C2).
    #[serde(default, skip_serializing_if = "Inbox::is_empty")]
    pub inbox: Inbox,
    /// N10: the sub-lead brief has not been sent yet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sub_brief: bool,
    /// U52: when this member's session was launched again after crashing on
    /// its own, oldest first, at most 20.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restarts: Vec<f64>,
    /// U52: the last crash seen by the dispatcher, for the lead's inbox item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crash: Option<f64>,
    /// U52: the lead already heard that this member's session stays stopped
    /// after too many crashes this hour.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub crash_told: bool,
    /// Task #334: the owner or the lead resumed this member while its agent
    /// had exited. The next tick launches the same conversation again (within
    /// the hourly restart bound) instead of leaving it stopped.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub relaunch_exited: bool,
}

/// U45: a model a member may run a task on, with its effort (empty: the
/// model's own).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct ModelChoice {
    pub model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub effort: String,
}

/// Task #6: the model and effort a running member's session must still switch
/// to. `members_update` records it; the dispatcher switches before the member's
/// next task and clears it. The values the session ran before are kept, so a
/// refused switch puts the member's model back where its session is.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct ModelSwitch {
    pub model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub effort: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from_model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from_effort: String,
}

/// Task #6: a member's start prompt changed while it ran. The dispatcher sends
/// it as the message that replaces its earlier instructions, after the clear
/// the owner asked for. `sent_at` is recorded before the message leaves, so a
/// crash during the send leaves it uncertain instead of sending it again.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct PromptChange {
    /// Clear the member's context before the message.
    #[serde(default)]
    pub clear_context: bool,
    /// The clear it started, while its confirmation is pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear: Option<PromptClear>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<f64>,
}

/// Task #6: a clear started before a changed start prompt, recorded so a later
/// tick - or a restarted worker - reads the same request instead of running it
/// again.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct PromptClear {
    pub request_id: String,
    pub run_id: String,
    pub conversation_id: String,
    pub started_at: f64,
}

/// Spec 0: what a ready task waits for that Zerus could not do on its own (a
/// refused or unconfirmed step before the hand-out: `{for:"zerus", on,
/// reason, at}`); the lead or the owner retries or reassigns it. U46: while a
/// context action runs, `state` is `waiting` and the request fields let a
/// later tick (or a restarted worker) read the same request instead of
/// running the action again.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct TaskWait {
    #[serde(rename = "for")]
    pub waits_for: String,
    #[serde(default)]
    pub on: Vec<String>,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub at: f64,
    /// `waiting` (U46: the context action runs), `refused` or `uncertain`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state: String,
    /// U46: the attempt whose preparation failed (0 while it waits).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub attempt: u32,
    /// U46: compact or clear, while it waits.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub action: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub request_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub run_id: String,
    /// U46: the conversation before the action: a clear shows a new one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub conversation: String,
    /// U46: when the action started; the 60 s confirmation bound counts from here.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub started_at: f64,
}

impl TaskWait {
    /// Spec 0: a step Zerus could not take; the lead or the owner retries it.
    pub(crate) fn zerus(on: &str, state: &str, reason: &str, at: f64) -> Self {
        TaskWait {
            waits_for: "zerus".into(),
            on: vec![on.to_owned()],
            reason: reason.chars().take(4000).collect(),
            at,
            state: state.to_owned(),
            ..Default::default()
        }
    }

    /// U46: the context action still runs; nobody has to act yet.
    pub(crate) fn running(&self) -> bool {
        self.state == "waiting"
    }
}

/// U21: one built-in command template Zerus ran for this attempt, outside the
/// member's sandbox.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct RunRecord {
    pub template: String,
    #[serde(default)]
    pub args: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    #[serde(default)]
    pub at: f64,
    #[serde(default)]
    pub ms: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
}

/// One commit Zerus made for an attempt (B4).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct CommitRef {
    pub sha: String,
    #[serde(default)]
    pub at: f64,
    #[serde(default)]
    pub by: String,
    #[serde(default)]
    pub files: usize,
}

/// A member's session as the dispatcher last saw it in the live listing.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct MemberSeen {
    pub at: f64,
    pub live: bool,
}

/// Task 98 (seen 2026-11-06): hand-outs refused because the member's composer
/// is not empty. A leftover paste of Zerus's own delivery is finished once; any
/// other draft or dialog is counted and, from `InputBlock::RAISE_AFTER`
/// refusals, raised once to the lead and the owner with the screen's last
/// lines. Never submitted for them.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct InputBlock {
    /// The first refusal of this episode, and the latest.
    pub since: f64,
    pub at: f64,
    pub count: u32,
    /// hgs's refusal, as the member card shows it.
    pub reason: String,
    /// The Terminal's last lines when the item was raised.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screen: Vec<String>,
    /// The lead and the owner have their item.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub raised: bool,
    /// Zerus's own leftover paste whose Enter it sent and whose delivery is
    /// unknown: never sent again.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub own_paste: String,
}

/// Task 139: a member's sources compared (see `look.rs`).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct MemberCheck {
    /// When the sources were last compared (the screen's read).
    pub at: f64,
    /// `agree`, `pending` (they disagree, within the grace) or `uncertain`.
    pub state: String,
    /// When they first disagreed in this episode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<f64>,
    /// Every source's view: hooks, record, screen, process, activity.
    #[serde(default)]
    pub sources: Value,
    /// The disagreements, in words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disagree: Vec<String>,
    /// The screen's last lines at that read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screen: Vec<String>,
    /// The lead was told of this episode.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub told: bool,
}

/// Task 113: a Terminal-only request raised to the owner after the defer time.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct TerminalEscalation {
    pub question_id: String,
    /// When Zerus raised it to the owner.
    pub at: f64,
    /// The Terminal's last lines then, at most `SCREEN_LINES`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screen: Vec<String>,
}

impl TerminalEscalation {
    pub(crate) const SCREEN_LINES: usize = 12;
}

impl InputBlock {
    /// The item rises with this many refusals.
    pub(crate) const RAISE_AFTER: u32 = 3;
}

/// U56: why Zerus marks a member stuck: no session event while it is busy.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct MemberStuck {
    pub at: f64,
    /// `compaction_timeout` (its phase was compacting) or `no_progress`.
    pub reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub phase: String,
    /// #728: the member's last progress Zerus saw (its last event, or the
    /// start of the tool call it waits in); its minutes are the stuck time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<f64>,
}

/// U62 (#717): one test result announced to the team: the record's id, its
/// suite and status, who asked and when. The log and excerpt stay in the test
/// database, read only when someone opens the record.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct TestNotice {
    pub record: String,
    pub suite: String,
    pub status: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub asked_by: String,
    #[serde(default)]
    pub at: f64,
    /// #718: reused, slower, over_time_budget, retried, crash, quarantined.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<String>,
    /// #718: the measured record a reused result stands for.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reused_from: String,
    /// #718: the suite's recorded seconds, to say how much slower it ran.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub recorded_seconds: f64,
    /// Task #24 (team ede724c4): the task the run was for, so the lead's
    /// inbox shows one item per task with its counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<u32>,
}

/// #718: a test item for the lead: a flaky suite to file a task for, or a
/// suite over its time or size budget. A dismissed one stays to count from.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct TestAlert {
    pub kind: String,
    pub suite: String,
    pub at: f64,
    pub detail: String,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub episodes: usize,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dismissed: bool,
}

/// A queue wait is independent of its human-readable diagnostic.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MergeWaitReason {
    Busy,
    Runners,
    Selection,
    Rollback,
    Ancestry,
    /// Task #267: the landing conflicts on the integration line; a resolve
    /// task waits for the reviewer, and later landings queue behind it.
    Conflict,
    #[default]
    #[serde(other)]
    Other,
}

impl MergeWaitReason {
    fn is_other(&self) -> bool { *self == Self::Other }
}

/// One task's untested merged range, never a synthetic test suite.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct MergeRange {
    pub task: u32,
    pub prev: String,
    pub merged: String,
}

/// Task #370: an accepted task the lead marked landed by a named commit on
/// the integration line (its work arrived as a cherry-pick).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Landed {
    pub commit: String,
    pub by: String,
    pub at: f64,
}

/// U62 (#760): an accepted report in the merge queue: the commit its author
/// reported, and, while its tests run, the lead branch before and after.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct MergeEntry {
    /// queued or testing.
    pub state: String,
    pub author: String,
    pub commit: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub branch: String,
    pub folder: String,
    pub at: f64,
    /// Who accepted it, and whether that was the owner.
    pub by: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub by_owner: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prev: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub merged: String,
    /// The test queue's requests of its affected set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<String>,
    /// Suites whose declared platforms exclude this computer, with no test
    /// runner of their class: the merge decides on the suites that ran.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_run: Vec<String>,
    /// Why it waits (the lead's folder has uncommitted changes).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    #[serde(default, skip_serializing_if = "MergeWaitReason::is_other")]
    pub wait_reason: MergeWaitReason,
    /// Open ranges included in these requests, closed only after they pass.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tested_ranges: Vec<MergeRange>,
    /// Original starts of not-run suites selected by an older open range.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub not_run_since: std::collections::BTreeMap<String, String>,
    /// A queued retry's lead-branch HEAD and tests-map fingerprint when the
    /// merge failed: a later tick retries only when one changed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub wait_head: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub wait_map: String,
    /// The lead's accept or retry asked to resume this queued retry even when
    /// neither fingerprint changed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retry: bool,
    /// Consecutive test-selection failures; the second returns the task.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub selection_failed: u32,
    /// Task #267: the resolve task a conflict on the integration line became;
    /// later landings wait behind it, and the landing retries once it is
    /// decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolve_task: Option<u32>,
    /// Task #267: the files the landing conflicted in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflict_files: Vec<String>,
    /// Task #370: the accepted commit carries a commit of a rejected attempt;
    /// the landing waits for a cherry-pick of the task's own commit.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub blocked: bool,
    /// Task #370: land by cherry-picking the accepted commit, never merging
    /// its branch.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cherry_pick: bool,
}

/// U62 (#760): one merge the queue kept, with the range it tested.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct MergeLog {
    pub task: u32,
    pub prev: String,
    pub merged: String,
    pub at: f64,
    #[serde(default)]
    pub suites: Vec<String>,
    /// Suites of another platform that had no runner here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_run: Vec<String>,
}

/// U62 (#760): a suite whose range is still open because this computer cannot
/// run it. It stays persisted while the bounded merge log may no longer name it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct OpenNotRun {
    /// The commit its open range starts at (the branch before its first
    /// not-run merge).
    pub since: String,
    /// The merge queue's not-run message, naming the suite and its platforms.
    pub message: String,
}

/// #718: a flaky suite's quarantine, shown to the lead and the owner until
/// it expires.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct TestQuarantine {
    pub suite: String,
    pub since: f64,
    pub until: f64,
    #[serde(default)]
    pub episodes: usize,
}

/// #718: Zerus proposes a full run once enough results were reused (or the
/// oldest reused measurement is too old), with its estimate.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct FullRunProposal {
    pub at: f64,
    pub reused: usize,
    pub oldest_at: f64,
    pub seconds: f64,
    #[serde(default)]
    pub unknown: usize,
    /// count or age.
    pub reason: String,
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

impl Member {
    /// Task #13 (team ede724c4): the member's session could not start and the
    /// dispatcher waits for a relaunch (`error` names why); the lead relaunches
    /// it itself.
    pub(crate) fn failed_to_start(&self) -> bool {
        self.session.is_none() && !self.error.trim().is_empty() && self.state != MemberState::Removed
    }

    /// A started DeepSeek member whose role chose a model or effort.
    pub(crate) fn needs_settings(&self) -> bool {
        self.agent == "dsh" && !self.settings_applied && (!self.model.is_empty() || !self.effort.is_empty())
    }
}

/// The owner's own sentence that let the lead approve a plan or a phase (F13):
/// the exact text, its time and the event it came from.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct OwnerWord {
    pub text: String,
    #[serde(default)]
    pub at: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub event: i64,
}

fn is_zero(value: &i64) -> bool {
    *value == 0
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

fn is_zero_f64(value: &f64) -> bool {
    *value == 0.0
}

/// Task 278: a job a member waits on, with its start: `kind` build (a Zerus
/// build), shell (a background job its hooks registered) or job (Background
/// work under a declared budget).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct MemberJob {
    pub kind: String,
    pub what: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub since: f64,
}

/// An MCP server or service a member's session runs (G7): shown on the member
/// card and never counted as Background work.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct MemberService {
    pub name: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub since: f64,
}

/// What a sub-team's plan and phases wait for (section 9.3): the parent lead
/// by default, or the owner.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SubteamApproval {
    #[default]
    Parent,
    Owner,
}

/// The team and task a sub-team was delegated from (section 9.1).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Parent {
    pub team: String,
    pub task: u32,
    /// The parent team's lead, who approves this sub-team's plan by default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub lead: String,
    /// The assignee whose session became the sub-team's lead.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub member: String,
}

/// Task #228: a QA member's scratch team names the parent team and the task
/// whose decision stops it. It is not a delegated sub-team: it has no parent
/// approval, no parent task report and no slot in the parent's `children`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct ScratchParent {
    pub team: String,
    /// The creating task, 0 when the member had none open.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub task: u32,
    /// The parent member whose session leads the scratch team.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub member: String,
}

/// Task #228: one scratch team a parent lead sees: created, then stopped when
/// the creating task was decided.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct ScratchChild {
    pub id: String,
    pub name: String,
    /// The creating task, 0 when the member had none open.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub task: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub member: String,
    pub at: f64,
    /// "created" or "stopped".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<f64>,
}

/// DCB-svn report (2026-11-12): a member's turn that ended on a provider
/// error while its session stays live: when, the error's words, its recovery
/// class (network, service and rate_limit are temporary; manual is not) and
/// whether Zerus already asked the member to continue after it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct TurnFailure {
    pub at: f64,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub class: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continued_at: Option<f64>,
}

impl TurnFailure {
    /// The same failure as `other`, whatever Zerus did about it since.
    pub(crate) fn same(&self, other: &TurnFailure) -> bool {
        self.at == other.at && self.detail == other.detail && self.class == other.class
    }

    /// A temporary error that a new turn may get past.
    pub(crate) fn temporary(&self) -> bool {
        matches!(self.class.as_str(), "network" | "service" | "rate_limit")
    }
}

/// A member's session stopped by a usage limit (H1); `resets_at` is present
/// only when the session reports a reset time.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct MemberLimit {
    pub at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<f64>,
    #[serde(default)]
    pub detail: String,
}

/// What a swapped member's next attempt needs to continue the work (H6): the
/// previous attempt, when it began, its partial report and its open questions.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Continuation {
    pub attempt: String,
    pub n: u32,
    pub since: f64,
    #[serde(default)]
    pub report: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<String>,
}

/// Task 209: one native context reading of a member's session.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct MemberContext {
    /// The tokens the agent's window holds now (`contextPressure`).
    pub used: u64,
    /// The agent's context window.
    pub limit: u64,
    /// `used` in whole percent of `limit`, rounded down.
    pub percent: u8,
    /// When Zerus read it (epoch seconds).
    pub at: f64,
}

impl MemberContext {
    pub(crate) fn of(used: u64, limit: u64, at: f64) -> Self {
        Self { used, limit, percent: super::context::percent(used, limit), at }
    }
    /// Whether the reading reached `threshold` percent of the window.
    pub(crate) fn reached(&self, threshold: u8) -> bool {
        self.limit > 0 && self.percent >= threshold
    }
    /// Whether the reading still stands for a claim (a turn moved it since).
    pub(crate) fn fresh(&self, now: f64) -> bool {
        self.at > 0.0 && now - self.at <= super::context::CLAIM_FRESH
    }
}

/// Task 209: a compaction, or a fresh conversation, Zerus waits to confirm
/// before the member continues. Kept on the member, so a restarted worker reads
/// the same request instead of running it again.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct MemberCompact {
    /// `compact`: the agent compacts its own conversation; `fresh`: the member
    /// gets a new conversation with a handoff brief.
    pub action: String,
    /// The native request id a compaction is confirmed by (empty for fresh).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub request_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub run_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub conversation: String,
    pub started_at: f64,
    /// The task whose open attempt continues after the compaction; 0 without one.
    #[serde(default)]
    pub task: u32,
    /// That attempt's id.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub attempt: String,
    /// Why Zerus did it: `threshold`, `claim` or `relaunch`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// Task 135: what a fresh session of a member still has to be told. Set by a
/// swap (a new agent, model or account ends the old session) and cleared when
/// the handoff brief goes out with the session's first message.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct HandoffNeed {
    /// `swap` today; the reason the brief is written.
    pub why: String,
    pub at: f64,
    /// The session the member was swapped away from.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from: String,
    /// What the swap changed, as `agent claude -> kimi`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<String>,
}

/// Task 135: one handoff brief, kept with the team (so it survives a restart of
/// the home computer). The latest of a task is kept while the team is; the older
/// versions of a task are dropped when the task closes.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Handoff {
    /// The task the brief is for; 0 when the member held none.
    pub task: u32,
    pub version: u32,
    /// The member's role.
    pub member: String,
    pub why: String,
    pub at: f64,
    pub text: String,
    /// The task's control questions the text cannot answer (empty when it can).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub misses: Vec<String>,
}

/// Task 135: a control question written with a task, and the strings an
/// answer to it holds: a fresh session given only the brief answers it when the
/// brief holds one of them (as the retrieval bench judges an answer).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Control {
    pub q: String,
    #[serde(default)]
    pub keys: Vec<String>,
}

/// A member's pending native question as Zerus read it (C2): the lead answers
/// it with `hgs team answer` by this exact id and hash, as before, but now the
/// inbox lists it with the rest instead of a message of its own.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct PendingQuestion {
    pub question_id: String,
    pub question_hash: String,
    #[serde(default)]
    pub approval: bool,
    #[serde(default)]
    pub at: f64,
    /// The card the answer command is built from: header, question and options.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub card: serde_json::Value,
}

/// The lead's last inbox message (C2): when it went and which items it named,
/// so one coalesced message replaces the per-item messages and a reminder can
/// still reach the lead later.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Inbox {
    #[serde(default)]
    pub told_at: f64,
    /// Item keys already named, oldest first, bounded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub told: Vec<String>,
    /// Task #24 (team ede724c4): told items that closed, with when, for
    /// `CLOSED_MEMORY_SECONDS`: one that opens again unchanged in that time
    /// (a member between two turns, a rework whose member is briefly idle)
    /// is not told again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed: Vec<ClosedItem>,
    /// Owner, 2026-11-17: reminders sent since the last list that named a new
    /// item; each one doubles the wait for the next.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub reminded: u32,
}

/// Task #24: a told inbox item that closed, and when.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct ClosedItem {
    pub key: String,
    pub at: f64,
}

/// Task #24: how long a closed item is remembered as told.
pub(crate) const CLOSED_MEMORY_SECONDS: f64 = 30.0 * 60.0;

impl Inbox {
    /// An empty record adds no key to an old team's JSON.
    pub(crate) fn is_empty(&self) -> bool {
        self.told_at == 0.0 && self.told.is_empty() && self.closed.is_empty() && self.reminded == 0
    }

    /// Task #24: whether the item `key` was told: it is still listed as told,
    /// or it closed less than `CLOSED_MEMORY_SECONDS` ago and opened again.
    pub(crate) fn knows(&self, key: &str, now: f64) -> bool {
        self.told.iter().any(|told| told == key) || self.closed.iter().any(|closed| closed.key == key && now - closed.at < CLOSED_MEMORY_SECONDS)
    }
}

/// The Background job a member's time budget is tied to (C12): its command and
/// when it started.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct JobRef {
    #[serde(default)]
    pub command: String,
    pub since: f64,
}

/// A member's declared time budget for its newest Background job (C12). The
/// budget never stops anything; it only says when the work is overdue.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Budget {
    pub label: String,
    pub minutes: u32,
    pub red_minutes: u32,
    pub declared_at: f64,
    /// Filled when the dispatcher ties the budget to live Background work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<JobRef>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub amber_told: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub red_told: bool,
}

impl Budget {
    /// How long the job has run, from its start or from the declaration.
    pub(crate) fn elapsed(&self, now: f64) -> f64 {
        now - self.job.as_ref().map(|job| job.since).unwrap_or(self.declared_at)
    }

    /// `ok`, `amber` (past the estimate) or `red` (past the red time) (C12).
    pub(crate) fn state(&self, now: f64) -> &'static str {
        let elapsed = self.elapsed(now);
        if elapsed >= self.red_minutes as f64 * 60.0 {
            "red"
        } else if elapsed >= self.minutes as f64 * 60.0 {
            "amber"
        } else {
            "ok"
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DependencyKind {
    /// The task waits until the dependency is Accepted.
    #[default]
    Hard,
    /// The task may start once the dependency is In progress.
    Soft,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Dependency {
    pub task: u32,
    #[serde(default)]
    pub kind: DependencyKind,
}

/// A task of the plan's work, or a check that runs a command in the member's
/// own folder and is decided by its exit code (C5).
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskKind {
    #[default]
    Task,
    Check,
}

impl TaskKind {
    /// An ordinary task keeps the old JSON shape: `kind` is skipped.
    pub(crate) fn is_task(&self) -> bool {
        matches!(self, Self::Task)
    }
}

/// The command a check task runs, its expected exit code and its time limit.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Check {
    pub command: String,
    #[serde(default)]
    pub expect_exit: i32,
    #[serde(default = "default_check_timeout")]
    pub timeout_minutes: u32,
}

fn default_check_timeout() -> u32 {
    30
}

/// What the member's check ran and printed: the last lines go to the report
/// and the decision is the exit code (C5).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct CheckResult {
    pub exit: i32,
    #[serde(default)]
    pub tail: String,
    pub at: f64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskStatus {
    Draft,
    #[default]
    Pending,
    Assigned,
    InProgress,
    Submitted,
    /// U36: a review stage of the task's pipeline reads the report.
    InReview,
    /// U36: a test stage of the task's pipeline runs its checks.
    InTest,
    Accepted,
    Rejected,
    Cancelled,
}

impl TaskStatus {
    pub(crate) fn finished(self) -> bool {
        matches!(self, Self::Accepted | Self::Rejected | Self::Cancelled)
    }
    /// U36: reported and in one of its pipeline's stages.
    pub(crate) fn in_stage(self) -> bool {
        matches!(self, Self::InReview | Self::InTest)
    }
    /// Reported and not decided: Submitted, or in a stage of its pipeline.
    pub(crate) fn reported(self) -> bool {
        self == Self::Submitted || self.in_stage()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Decider {
    Lead,
    Human,
    /// U48: a pipeline's land stage accepted the task.
    Zerus,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AttemptState {
    /// Recorded before the hand-off is sent.
    #[default]
    Delivering,
    Delivered,
    /// The hand-off may or may not have reached the agent; a human decides.
    Unconfirmed,
    /// Refused before anything reached the agent.
    Refused,
    /// Replaced by a reassignment, retry or take-over.
    Revoked,
    Reported,
    Rejected,
    /// Task 160: parked on an outside event: not open (its member is free),
    /// given back in the same attempt when the condition holds.
    Parked,
}

impl AttemptState {
    /// The attempt still owns its task.
    pub(crate) fn open(self) -> bool {
        matches!(self, Self::Delivering | Self::Delivered | Self::Unconfirmed)
    }
}

/// Forwarding a report to the lead, like a hand-off, is never retried after
/// it may have arrived.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Notice {
    #[default]
    None,
    Pending,
    Sending,
    Sent,
    Unconfirmed,
}

impl Notice {
    fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

/// Who finished the team, and with what summary.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Finish {
    /// The lead or you.
    pub by: String,
    pub summary: String,
    pub at: f64,
}

/// D2: a finding a report filed, kept until it is fixed or dropped.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Finding {
    /// f1, f2, ... in filing order.
    pub id: String,
    pub from_task: u32,
    pub severity: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub scenario: String,
    /// The task that caused it, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<u32>,
    pub state: String,
    pub by: String,
    pub at: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// N17: the owner's replan proposal, kept with the finding it answers.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub proposal: String,
    /// N18: the draft phase a `fix_phase` finding is marked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<u32>,
}

impl Finding {
    /// N18: still to be dealt with - open, reopened, urgent, or waiting in a
    /// replan or a draft phase. These block a finish when critical and count
    /// as open in the summary.
    pub(crate) fn unresolved(&self) -> bool {
        matches!(self.state.as_str(), "open" | "reopened" | "urgent" | "replan" | "fix_phase")
    }

    /// N14: fixed and dropped findings are closed for good.
    pub(crate) fn closed(&self) -> bool {
        matches!(self.state.as_str(), "fixed" | "dropped")
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Report {
    pub text: String,
    pub at: f64,
    /// Zerus took the member's last reply as the report (A2); the member may
    /// replace it while the task is undecided (A6).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub captured: bool,
    /// Captured texts this report replaced, the newest three (A6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaced: Vec<ReplacedReport>,
    /// D1: the structured report object, validated and bounded; None for an
    /// unstructured report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<serde_json::Value>,
    /// D1: a report without a structured object, shown as such in status.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unstructured: bool,
    /// #718: the test lines and functions the report's change adds and
    /// removes, read by Zerus from the member's folder (bloat guard).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests: Option<serde_json::Value>,
}

/// A report text that a member's real report replaced (A6).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct ReplacedReport {
    pub text: String,
    pub at: f64,
}

/// A member's question to the lead about its open attempt (A3): one message
/// per question, answered once by the lead or the owner.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Question {
    pub id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub at: f64,
    /// Empty while the question waits for an answer.
    #[serde(default)]
    pub answer: String,
    #[serde(default)]
    pub answered_at: Option<f64>,
    /// The lead or you, who answered.
    #[serde(default)]
    pub by: String,
    /// The answer's message to the member: Pending until it reaches it (A3).
    #[serde(default, skip_serializing_if = "Notice::is_none")]
    pub notice: Notice,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notice_id: String,
    /// Section 9.6: the parent question this one was escalated as; the
    /// parent's answer is copied back here.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub forwarded: String,
    /// N24: on a parent-side question, the child question it carries (created
    /// by an escalation); the answer travels on down to it. A team stored
    /// before this field keeps that child id in `forwarded`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
}

impl Question {
    /// Still waiting for an answer. A question closed with its attempt or a
    /// finished task has `answered_at` and no answer.
    pub(crate) fn open(&self) -> bool {
        self.answer.is_empty() && self.answered_at.is_none()
    }
}

/// A task kept back from the dispatcher until the lead releases it (A5): a
/// rejection may return a task to a member who is not ready for it.
/// Task 173: a member's work outside any hand-out.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Unassigned {
    pub since: f64,
    pub what: String,
    /// Task #227: the task the member's screen or branch shows it working on,
    /// so the lead's item names it and one command binds the work to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<u32>,
}

/// Task 160: a task parked on a named outside condition, with an estimate.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Park {
    /// The parked attempt and its member: the task goes back to them.
    pub attempt: String,
    pub member: String,
    /// Who parked it (the member, the lead or you) and when.
    pub by: String,
    pub at: f64,
    pub condition: ParkCondition,
    /// When the parker expects the condition to hold (an estimate).
    pub eta: f64,
    /// The member's notes for its return.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// waiting, met or failed.
    #[serde(default)]
    pub state: String,
    /// What the check saw: why it held or failed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub checked_at: f64,
}

/// Task 160: what a park waits for. Zerus checks each itself, bounded.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ParkCondition {
    /// A GitHub Actions run of the fork finishing (`repo` is OWNER/REPO).
    Ci { repo: String, run: u64 },
    /// A build installed: at least `version`, at least Dev `build`, or exactly
    /// `commit`; `program` is an installed desktop answering --build-info
    /// (empty: the hgs the team worker runs).
    Build {
        #[serde(default)]
        version: String,
        #[serde(default)]
        build: u64,
        #[serde(default)]
        commit: String,
        #[serde(default)]
        program: String,
    },
    /// Another task accepted.
    Task { task: u32 },
    /// The owner's answer on a card.
    Card { proposal: String },
    /// A time, in seconds since 1970.
    Time { at: f64 },
    /// Task #23 of team c536f0e5 (owner, 2026-11-23): the member's own run
    /// (`hgs team run`) finishing: its result in the member's outbox.
    Run { request: String },
}

impl Default for ParkCondition {
    fn default() -> Self {
        Self::Time { at: 0.0 }
    }
}

/// Task 156: one owner decision: the question, the choice, why, who and when,
/// with the refs it concerns; superseded by a later one on the same question.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Decision {
    pub id: String,
    pub question: String,
    pub choice: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub by: String,
    #[serde(default)]
    pub at: f64,
    /// The card it answered, if any.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub card: String,
    /// The spec (and section) it amends, such as docs/lore.md#2.1.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub spec: String,
    /// The output paths and tasks it concerns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<u32>,
    /// Terms a report must not add (a dependency, a tool), lowercase.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbids: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub superseded_by: String,
    /// The question's content words: a later decision with the same supersedes it.
    #[serde(default)]
    pub topic: String,
    /// Task #279: "rule" for a standing rule the owner stated, empty for an
    /// ordinary decision; a rule is recalled in every lead brief and in the
    /// hand-out of every task it binds.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// Task #279: the owner's message a standing rule cites: its id in the lead
    /// session's journal, the quote it was verified by, and when it was said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_message: Option<serde_json::Value>,
}

/// Task 156: lore's hits over decisions for one task, and what they were for.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct DecisionHits {
    pub hash: String,
    pub ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Hold {
    /// The lead or you.
    pub by: String,
    pub at: f64,
    #[serde(default)]
    pub reason: String,
}

/// Spec 1: one step Zerus runs for a member before a hand-out (model,
/// context, branch). `result` is `done`, `skipped`, `refused` or `uncertain`
/// (U45: `pending` while a model switch waits for its confirmation); `reason`
/// names the refusal or why the step was skipped. `member` names whom a step
/// still on the task is for; the attempt's own steps leave it empty.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct PrepareStep {
    pub step: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub member: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub to: String,
    #[serde(default)]
    pub at: f64,
    pub result: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Attempt {
    pub n: u32,
    /// The hand-off's request id.
    pub id: String,
    pub member: String,
    #[serde(default)]
    pub state: AttemptState,
    /// U46: what Zerus did to the member before this hand-out - the context
    /// action today - and whether it finished, was skipped, refused or stayed
    /// unconfirmed (spec 1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prepare: Vec<PrepareStep>,
    /// #728 L2: when the silent check last read a dsh session for this attempt;
    /// the stuck check reads it at most once a minute too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silent_read_at: Option<f64>,
    /// #630 C2: a stage return or a send-back closed this attempt; the
    /// implementer keeps the task and is not counted as failed for it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pipeline_return: bool,
    /// Task 128: the exact reason this attempt was sent back for rework (the
    /// lead's or the owner's reject, or a failed check), at most 2000
    /// characters; the team memory's fault keeps it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rework_reason: String,
    /// #684 N1: the folder's HEAD and branch when this attempt reported, so a
    /// later stage checks that commit and never the folder's HEAD at the
    /// stage's own start.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder_head: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder_branch: String,
    /// Task 165: how many hand-outs this one refused in a row, and why.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub refusal_count: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub refusal_reason: String,
    /// The member's state when it refused; a change retries at once.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub refusal_state: String,
    #[serde(default)]
    pub sent_at: f64,
    #[serde(default)]
    pub delivered_at: Option<f64>,
    /// The first dispatcher tick after delivery that showed the member working
    /// on it (A1); absent until it really started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<f64>,
    /// The lead was told once that the member did not start in time (A1).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub start_warned: bool,
    /// When the member's turn after the hand-off ended, for No report.
    #[serde(default)]
    pub turn_ended_at: Option<f64>,
    /// Task 133: the end of the last turn Zerus wrote (or skipped) a pulse
    /// for; the next pulse summarizes only what came after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pulse_at: Option<f64>,
    /// The first tick that saw the capture gate open: the report timeout counts
    /// from here, not from the reply (C2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_ready_at: Option<f64>,
    /// The first tick that saw the finished turn blocked by an open question or
    /// by Background work; the lead is told once it lasts too long (N5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_since: Option<f64>,
    /// The member's session reply id at `turn_ended_at`: a newer turn is never
    /// taken as this attempt's report (A2).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub turn_reply: String,
    /// The member's last reply at `turn_ended_at` (or at No report): the lead
    /// reads it and may use it as the report explicitly. Zerus never stores it
    /// as the report by itself (owner, 2026-11-07).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_reply: String,
    /// Task 173: the lead's messages about this attempt ({at, via, text}),
    /// newest last, at most 20: a lead message naming the task is bound here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lead_messages: Vec<serde_json::Value>,
    /// Task 161: when Zerus last looked for a wait loop in this attempt's
    /// work (its runs and repeated checks), at most every five minutes.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub wait_checked_at: f64,
    /// Task #413: the member's folder HEAD when a repeated check was last
    /// seen; the check is a wait loop only while that state stands.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub wait_head: String,
    /// Task 156: the decisions this attempt's member was told of while it worked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions_told: Vec<String>,
    /// Task 156: the decisions its report contradicts ({decision, term,
    /// choice}), found in its changes before accept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_flags: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub decisions_scanned: bool,
    /// The member's turn ended long enough ago without a report.
    #[serde(default)]
    pub no_report: bool,
    /// Task 209: a reply that claimed there was no context left while the
    /// member's meter said otherwise was answered with the meter; the attempt
    /// stays open and its report is still what ends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_answered_at: Option<f64>,
    /// Task 36 (audit P8): the Zerus build the member's session was seen
    /// Building on for this attempt (a claimed `hgs team run`, or `hgs run`):
    /// its request id, or "build" when the listing names none. While it is
    /// set and the build has settled, the finished turn is not captured: the
    /// member is woken once to read the result and go on.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub awaited_run: String,
    /// What that build runs, for the wake message.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub awaited_label: String,
    /// The newest session event the dispatcher read for this attempt when the
    /// listing's `last_event_at` did not advance (a DeepSeek session's native
    /// tool events, #628). It keeps the silent check from reading the session
    /// on every tick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_at: Option<f64>,
    /// The lead was reminded once that this report still waits for a decision
    /// (C4); absent until it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminded_at: Option<f64>,
    /// U37: when the lead or the owner accepted or rejected this attempt;
    /// absent on an old record and while undecided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<f64>,
    /// The check this attempt ran, when its task is a check task (C5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckResult>,
    #[serde(default)]
    pub report: Option<Report>,
    /// An interim report on this open attempt (progress or a question, not a
    /// delivery): the task stays In progress and nothing enters review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interim: Option<Report>,
    /// Questions this attempt asked the lead, oldest first (A3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<Question>,
    /// The report's message to the lead.
    #[serde(default)]
    pub notice: Notice,
    #[serde(default)]
    pub notice_id: String,
    #[serde(default)]
    pub error: String,
    /// What the lead was told about this attempt's member on its own
    /// (stopped, no_report), once each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub told_lead: Vec<String>,
    /// The member the task was assigned to when Zerus gave it to this
    /// attempt's member instead (a free member with the same role).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rebalanced_from: String,
    /// Commits Zerus made for this attempt (B4), newest last, at most 20.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<CommitRef>,
    /// #538 N5: the reported files' state at the report itself, so a later stage
    /// hashes the report's moment rather than its own delivery.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub report_files: Vec<Value>,
    /// U21: the built-in command templates Zerus ran for this attempt, newest
    /// last, at most 20.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<RunRecord>,
}

impl Attempt {
    /// The attempt's first question still waiting for an answer (A3).
    pub(crate) fn open_question(&self) -> Option<&Question> {
        self.questions.iter().find(|question| question.open())
    }
}

/// Task 81: where a task should start and end by the estimates when its plan
/// was approved. A moment is None where an estimate on the way was missing:
/// lateness against it is unknown, never a guess.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Baseline {
    /// When the baseline was fixed.
    pub at: f64,
    /// The task's estimate then, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<f64>,
    /// The tasks it was planned to follow: its hard dependencies and the
    /// member's previous task. Their delay is the task's inherited lateness.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<u32>,
    /// Task 94: derived after the fact for a phase already running.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub backfilled: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Task {
    /// Full history is in team_task_archive; these summaries remain in the plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<ArchivedTask>,
    pub id: u32,
    /// The phase of the plan: the approved initial plan is 1, `extend`
    /// appends later phases.
    #[serde(default = "default_phase")]
    pub phase: u32,
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// Repo-relative file or directory paths. Empty on older tasks: rebalance
    /// keeps them with a member that already has work in progress.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output: Vec<String>,
    /// A plan task, or a check that runs a command (C5).
    #[serde(default, skip_serializing_if = "TaskKind::is_task")]
    pub kind: TaskKind,
    /// The check a check task runs; absent on a plan task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<Check>,
    #[serde(default)]
    pub criteria: Vec<String>,
    /// Task 154: what the task needs of its member's session (rights.rs
    /// CAPABILITIES); absent, Zerus reads the needs from the task's words, and
    /// an empty list says it needs nothing beyond editing and building.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs: Option<Vec<String>>,
    /// Task 135: questions a fresh session must answer from the task's handoff
    /// brief alone, each with the strings an answer holds. At most 12.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub control: Vec<Control>,
    #[serde(default)]
    pub deps: Vec<Dependency>,
    /// Task #289: dependencies the lead kept after they were cancelled: the
    /// task does not need their cancelled work, so the inbox notice clears.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept_cancelled: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<Prerequisite>,
    /// Task #60 (owner, 2026-11-06): the task this one was added for (a missed
    /// item or a bug it produced), in the same team. Unlike `deps` it never
    /// blocks or gates: the task is placed at least in its origin's layer and
    /// carries the link. Absent on tasks without one and on older records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<u32>,
    /// Task #229 (owner, 2026-11-07): what a spawned follow-up is. "crit"
    /// blocks using the phase's result: it joins its origin's running phase,
    /// depends on the origin, and holds the phase's acceptance, its close and
    /// the release candidate. "minor" is a shortcoming or missing
    /// functionality that blocks nothing: it joins the next phase that has not
    /// started, or a Minor phase. Empty on every task not spawned so, and on
    /// older records.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub severity: String,
    #[serde(default)]
    pub assignee: String,
    /// The lead or you chose this member: Zerus does not give the task to
    /// another member with the same role.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// A non-final rejection reserves this task's member until it is decided
    /// or explicitly reassigned. Missing on tasks saved before rework existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rework_since: Option<f64>,
    /// Only an explicit fresh rejection may bypass shared Output on its next
    /// attempt. Automatic send/preparation refusals never grant this policy.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fresh_retry: bool,
    /// Owner 2026-10-31: the lead's time estimate in minutes. Absent until the
    /// lead sets one; a task without one shows Zerus' own estimate from the
    /// per-computer history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate_minutes: Option<f64>,
    /// `history`, `role`, `lead` or `none`: where the estimate came from.
    #[serde(default, skip_serializing_if = "no_estimate_source")]
    pub estimate_source: String,
    /// Task 81: the schedule this task was planned on when it was approved (or
    /// added to a running plan), fixed from then on: lateness is measured
    /// against it. Absent on a task saved before, or one already running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<Baseline>,
    /// U27: the visual lane this task is drawn in, set by the lead or the owner;
    /// it changes nothing about hand-out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<Lane>,
    /// U59: when this task is the free phase's integration-and-build task.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub phase_build: bool,
    /// U62 (#760): the accepted report waiting in the merge queue, or under
    /// its affected tests on the merged commit; the task stays undecided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge: Option<MergeEntry>,
    /// Task #370: the lead marked this accepted task landed by a named line
    /// commit (a cherry-pick of its work); journaled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed: Option<Landed>,
    /// At most one open range per task; repeated returns widen it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untested_merge: Option<MergeRange>,
    /// A phase build's exact named ranges, bounded to one batch of 500 tasks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build_merge_ranges: Vec<MergeRange>,
    #[serde(default)]
    pub status: TaskStatus,
    /// Held back from the dispatcher until the lead releases it (A5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<Hold>,
    /// Task 160: parked on an outside event; Zerus watches the condition and
    /// gives the task back to the same member in the same attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub park: Option<Park>,
    #[serde(default)]
    pub attempts: Vec<Attempt>,
    #[serde(default)]
    pub decided_by: Option<Decider>,
    #[serde(default)]
    pub decision_reason: String,
    /// D2: what a reopen replaced, newest last (an accepted task's decision).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Value>,
    /// D2: the finding text a reopened task hands out with its next attempt.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reopen_note: String,
    /// H6: the hand-out after a swap carries the previous attempt's partial
    /// report and open questions; cleared once the new attempt is delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<Continuation>,
    /// Section 9.4: the sub-team delegated from this task; its status is
    /// derived for `hgs team status`, not stored here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subteam: Option<String>,
    /// U29: the task joined its phase after the phase started, on the owner's
    /// word twice; outside the phase's original scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_late: Option<AddedLate>,
    /// U36: the pipeline preset the plan chose: empty takes the role's or the
    /// template's default, "none" takes none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pipeline: String,
    /// U36: the preset as it was when the task was assigned, with its rounds
    /// and stage runs; a later catalog edit does not change it. None keeps
    /// today's flow (an old team, "none", a check or the lead's own task).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_run: Option<PipelineRun>,
    /// U45: the models allowed for this task (`model` or `model@effort`);
    /// empty: any model of its member.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// U45: a task for a role: with an empty assignee it goes to a free
    /// member of this role whose models allow the task's.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    /// Spec 0: a step before the hand-out was refused or never confirmed (U45
    /// model, U46 context, U48 branch), or U46's context action still runs;
    /// the dispatcher hands it out again only after a retry or a reassignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waits: Option<TaskWait>,
    /// Spec 1: the steps taken for the next hand-out, newest last, at most 20;
    /// they move to the attempt that hands it out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prepare: Vec<PrepareStep>,
}

/// Frozen display metadata, computed before the task history leaves the live record.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct ArchivedTask {
    pub accepted_at: Option<f64>,
    pub started_at: Option<f64>,
    pub merge_commit: String,
    pub timing: serde_json::Value,
}

/// One tick's index of each member's in-progress tasks, built once rather
/// than scanning the full task list for every candidate member.
pub(crate) struct InProgressOutputs<'a> {
    members: std::collections::BTreeMap<&'a str, Vec<&'a Task>>,
}

impl<'a> InProgressOutputs<'a> {
    pub(crate) fn new(team: &'a Team) -> Self {
        let mut members = std::collections::BTreeMap::<&str, Vec<&Task>>::new();
        for task in &team.tasks {
            if !task.assignee.is_empty() && matches!(task.status, TaskStatus::Assigned | TaskStatus::InProgress) {
                members.entry(&task.assignee).or_default().push(task);
            }
        }
        Self { members }
    }

    /// Task 155/259: the task may not move to `member`, which already has
    /// work in progress writing the same files. Task 259: a folder-level
    /// output is advisory, so only files and module paths hold; a task
    /// without declared outputs stays with a member that already has work.
    pub(crate) fn blocks_for(&self, member: &str, task: &Task) -> bool {
        self.members.get(member).is_some_and(|active| active.iter().any(|other| {
            other.id != task.id && (task.output.is_empty() || task.narrow_output_overlaps(other))
        }))
    }

    pub(crate) fn blocks(&self, task: &Task) -> bool {
        self.blocks_for(task.assignee.as_str(), task)
    }
}

/// U36: one stage of a task's pipeline snapshot.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct PipelineStage {
    /// review, test, command (U47) or land (U48).
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    /// A pinned member; otherwise the first free member with `role`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub member: String,
    /// U47/U48: what a command or land stage runs, from the owner's catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Check>,
    /// U48: a land stage's exit codes and their outcomes (accepted, returned,
    /// held); empty reads as 0 accepted, any other held.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub exits: std::collections::BTreeMap<String, String>,
}

impl PipelineStage {
    /// U47/U48: Zerus runs this stage itself, with no member.
    pub(crate) fn runs_command(&self) -> bool {
        matches!(self.kind.as_str(), "command" | "land")
    }
}

/// U36: what became of one stage run.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StageState {
    /// Waits for a stage member (stage delivery gives it out).
    #[default]
    Waiting,
    /// Given to its member, who reports a verdict.
    Delivered,
    Passed,
    Returned,
    /// The lead or the owner skipped it.
    Skipped,
    /// Ended without a verdict: the task was accepted, rejected or sent back.
    Revoked,
}

impl StageState {
    pub(crate) fn open(self) -> bool {
        matches!(self, Self::Waiting | Self::Delivered)
    }
}

/// U36: one stage of one round: who did it, when, and its verdict. Its span
/// (`delivered_at` or `at` to `ended_at`) joins the task's working time (U37).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct StageRun {
    /// The request id its member reports with, as an attempt id.
    pub id: String,
    /// Index into the pipeline's stages.
    pub stage: usize,
    pub kind: String,
    pub round: u32,
    /// The implementer's attempt this run checks.
    pub attempt: u32,
    /// Empty until stage delivery gives it to a member.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub member: String,
    #[serde(default)]
    pub state: StageState,
    /// When the stage began waiting for its member.
    #[serde(default)]
    pub at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<f64>,
    /// The stage member's report, or the lead's or owner's reason.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// A return's findings, one per line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<String>,
    /// Who ended it: the stage's role, Lead, You, Pipeline or Zerus.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    /// #532: the folder the stage works in: the task's own folder, or its
    /// detached checkout.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// A detached checkout Zerus made for this test, removed after it, and the
    /// repository it belongs to.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub checkout: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub checkout_repo: String,
    /// This test holds the task's folder until it ends.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub holds: bool,
    /// What changed: the attempt's commits, or its reported files with their
    /// size and hash when the stage began.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<serde_json::Value>,
    /// A test's own temporary folder, removed after the stage.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub temp: String,
    /// The command templates the stage ran, newest last, at most 20.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<RunRecord>,
    /// U47: a command stage's exit code and output tail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckResult>,
    /// U47: the folder's commit the command ran on.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub commit: String,
    /// U47: the same tail on the same commit as the stage's previous run: it
    /// went to the lead instead of the implementer.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub flaky: bool,
    /// U48: what a land stage's exit code meant (accepted, returned, held).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub outcome: String,
    /// #538 N4: the stage closed while a run it started may still go in
    /// its folder; the folder and the checkout stay held until that run ends.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub awaiting_run: bool,
    /// #630 N6: the stage member's questions to the lead, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<Question>,
    /// #630 N7: why the stage went back to Waiting (a removed or paused
    /// member, a hand-off never confirmed); empty while it has its member.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stalled: String,
    /// #635 N2/#727: the member a stall kept: its live run still uses the
    /// folder and checkout, or the verdict it owed still arrives while the
    /// stage waits. `member` is empty so a peer may take the stage.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stalled_member: String,
    /// #635 N1: the stage's member ended its turn without a verdict while the
    /// run stays Delivered; the lead hears it once.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub silent: String,
    /// #630 N7: the hand-off's delivery was not confirmed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unconfirmed: bool,
    /// #581 N1: the steps taken for this stage's hand-out (model, context), in
    /// order, as an attempt's prepare carries them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prepare: Vec<PrepareStep>,
    /// #581 N1: a context action this stage hand-out started and waits for;
    /// the stage is reserved for its member until it confirms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<TaskWait>,
}

/// U36: a task's pipeline snapshot and progress.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct PipelineRun {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub stages: Vec<PipelineStage>,
    pub max_rounds: u32,
    /// hold or checkout.
    pub folder: String,
    /// lead, sub_lead or auto.
    pub accept: String,
    /// The round of the report being checked: 1 for the first report, one
    /// more after each return or send-back.
    #[serde(default = "first_round")]
    pub round: u32,
    /// #538 N5: the folder is held from the report, because a test stage
    /// ahead may need it (the hold rule, or a checkout that will fall back).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hold_from_report: bool,
    /// Every stage run, oldest first, at most 64.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<StageRun>,
    /// #538 L2: the stage returns among the runs trimmed away, so the round
    /// limit still counts them.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub trimmed_returns: u32,
}

fn first_round() -> u32 {
    1
}

impl PipelineRun {
    /// The open stage run, if the task is in a stage.
    pub(crate) fn open_run(&self) -> Option<&StageRun> {
        self.runs.iter().rev().find(|run| run.state.open())
    }
    pub(crate) fn open_run_mut(&mut self) -> Option<&mut StageRun> {
        self.runs.iter_mut().rev().find(|run| run.state.open())
    }
    /// The rounds stage members returned; the round limit counts these.
    pub(crate) fn returned(&self) -> u32 {
        self.trimmed_returns + self.runs.iter().filter(|run| run.state == StageState::Returned).count() as u32
    }
}

/// U29: who added a task to a running phase, when, and the owner's two
/// sentences: the request and the confirmation after the warning.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct AddedLate {
    pub by: String,
    #[serde(default)]
    pub at: f64,
    #[serde(default)]
    pub request: String,
    #[serde(default)]
    pub confirm: String,
}

/// U29: an addition to a running phase that waits for the owner's
/// confirmation of the warning. One per phase; it expires after 30 minutes.
/// Task 162: one change of the plan the lead made inside the approved goal.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct LeadChange {
    /// "lcN", in order.
    pub id: String,
    pub op: String,
    /// What changed, in words ("added #12 Fix the login crash to phase 3").
    pub summary: String,
    pub by: String,
    pub at: f64,
    /// The tasks it changed as they were before it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks_before: Vec<Task>,
    /// The tasks it added.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<u32>,
    /// The phases it changed as they were before it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases_before: Vec<Phase>,
    /// Task #229: the phases it opened (a Minor phase), removed on a revert.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases_added: Vec<u32>,
    /// The owner looked at it in the digest (it leaves Waiting).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub seen: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverted_at: Option<f64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reverted_by: String,
    /// Task #4 (team ede724c4): the ids of `tasks_before`, kept in the team's
    /// head while the snapshots live in `team_lead_changes` (read on demand).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<u32>,
    /// The numbers of `phases_before`, as `changed` is for the tasks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases_changed: Vec<u32>,
}

impl LeadChange {
    /// The tasks it changed: from the snapshots when they are loaded, else
    /// from the ids the head keeps.
    pub(crate) fn changed_tasks(&self) -> Vec<u32> {
        if self.tasks_before.is_empty() { self.changed.clone() } else { self.tasks_before.iter().map(|task| task.id).collect() }
    }

    /// The phases it changed, as `changed_tasks` is for the tasks.
    pub(crate) fn changed_phases(&self) -> Vec<u32> {
        if self.phases_before.is_empty() { self.phases_changed.clone() } else { self.phases_before.iter().map(|phase| phase.n).collect() }
    }

    /// Whether it changed tasks or phases whose snapshots are not loaded.
    pub(crate) fn history_missing(&self) -> bool {
        (self.tasks_before.is_empty() && !self.changed.is_empty()) || (self.phases_before.is_empty() && !self.phases_changed.is_empty())
    }
}

/// Task 162: at most this many lead changes are kept (the oldest seen or
/// reverted ones leave first).
pub(crate) const LEAD_CHANGES_KEPT: usize = 300;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct PendingAddition {
    pub id: String,
    pub phase: u32,
    pub tasks: Vec<Task>,
    #[serde(default)]
    pub by: String,
    #[serde(default)]
    pub request: String,
    /// When Zerus answered with the warning; the confirmation must be newer.
    #[serde(default)]
    pub warned_at: f64,
}

/// U51: an owner-created key that lets one process add an urgent phase. The
/// team stores only the key's hash; the key is shown once when it is created.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Intake {
    /// `i1`, `i2`, ... in creation order.
    pub id: String,
    #[serde(default)]
    pub label: String,
    /// The SHA-256 of the key, hex.
    pub hash: String,
    #[serde(default)]
    pub created: f64,
}

/// U27: a visual lane of a phase, in order: the tasks drawn in it read as one
/// row; the label names the row ("Checkpoint 1").
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Lane {
    pub n: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

/// Task 259: a top-level container of areas (`client/src`, `crates/replica`,
/// `mobile/android`), as opposed to a module path (`src/state/teams`).
const CONTAINER_AREAS: [&str; 6] = ["client", "crates", "mobile", "services", "tray", "packaging"];

/// Task 259: dotless root files, which are files and not whole areas.
const ROOT_FILES: [&str; 9] = ["Makefile", "Dockerfile", "Containerfile", "LICENSE", "VERSION", "DEV_VERSION", "hgs", "AUTHORS", "COPYING"];

/// Task 259 (owner, 2026-11-08): roles that do the same kind of work share
/// their first word ("Frontend SolidJS" and "Frontend DeepSeek"), so a ready
/// task may pass between them. Within it the exact role is only the
/// preference; an unrelated role is no candidate.
pub(crate) fn related_roles(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a.eq_ignore_ascii_case(b) || role_family(a) == role_family(b)
}

fn role_family(role: &str) -> String {
    role.split(|c: char| !c.is_alphanumeric()).find(|part| !part.is_empty()).unwrap_or_default().to_lowercase()
}

/// The output prefix of a lore area (`lore:AREA`).
pub(crate) const LORE_OUTPUT: &str = "lore:";

impl Task {
    pub(crate) fn requests_fresh_retry(&self) -> bool {
        self.fresh_retry && self.failed_by(&self.assignee)
    }
    pub(crate) fn failed_by(&self, member: &str) -> bool {
        self.attempts.iter().any(|attempt| attempt.member == member
            && matches!(attempt.state, AttemptState::Rejected | AttemptState::Refused)
            && !super::pipeline::pipeline_returned(attempt))
    }
    /// Paths are normalized at task creation/editing; directory containment
    /// compares components so `src/api` never contains `src/apis`.
    pub(crate) fn output_overlaps(&self, other: &Task) -> bool {
        self.output.iter().any(|a| other.output.iter().any(|b| Self::paths_overlap(a, b)))
    }
    /// One output path contains the other, compared by components.
    pub(crate) fn paths_overlap(a: &str, b: &str) -> bool {
        let a: Vec<_> = a.split('/').filter(|part| !part.is_empty() && *part != ".").collect();
        let b: Vec<_> = b.split('/').filter(|part| !part.is_empty() && *part != ".").collect();
        a.starts_with(&b) || b.starts_with(&a)
    }
    /// Task 259 (owner, 2026-11-08): a whole area, not a file or a module.
    /// `src`, `tray` and a top-level folder; or an area directly inside a
    /// container of areas (`client/src`, `crates/replica`, `mobile/android`).
    /// A folder-level output is advisory: it never holds another task, since
    /// it would hold every task touching that area while it is open. Plan
    /// validation warns and asks for the files or module paths instead.
    pub(crate) fn broad_output(path: &str) -> bool {
        // RogueDOD (2026-11-11): a lore area is one exact resource, never broad.
        if Self::lore_area(path).is_some() {
            return false;
        }
        let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty() && *part != ".").collect();
        match parts.as_slice() {
            // `.` covers the whole repository: the broadest area there is.
            [] => true,
            [area] => Self::folder_like(area) && !ROOT_FILES.contains(area),
            [container, area] => CONTAINER_AREAS.contains(container) && Self::folder_like(area),
            _ => false,
        }
    }
    /// A path component that names a folder rather than a file: it has no
    /// extension (`src`, `crates`; `.github` counts as a folder). A dotless
    /// root file such as `Makefile` is excused in `broad_output`.
    fn folder_like(component: &str) -> bool {
        !component.rsplit_once('.').is_some_and(|(name, ext)| !name.is_empty() && !ext.is_empty())
    }
    /// RogueDOD (2026-11-11): `lore:AREA` names the lore area a task writes
    /// records into. Two open tasks with the same area are held apart like two
    /// that write one file, so parallel members never fill one area at once.
    pub(crate) fn lore_area(path: &str) -> Option<&str> {
        path.strip_prefix(LORE_OUTPUT)
    }
    /// The lore areas among the task's outputs.
    pub(crate) fn lore_areas(&self) -> Vec<&str> {
        self.output.iter().filter_map(|path| Self::lore_area(path)).collect()
    }
    /// The task's outputs that are files or module paths: the ones that hold.
    pub(crate) fn narrow_outputs(&self) -> impl Iterator<Item = &str> + '_ {
        self.output.iter().map(String::as_str).filter(|path| !Self::broad_output(path))
    }
    /// The task's folder-level outputs, for the plan's warning.
    pub(crate) fn broad_outputs(&self) -> Vec<&str> {
        self.output.iter().map(String::as_str).filter(|path| Self::broad_output(path)).collect()
    }
    /// Task 259: an overlap of two files or module paths; the only output
    /// overlap that holds two tasks apart. A folder-level path only advises.
    pub(crate) fn narrow_output_overlaps(&self, other: &Task) -> bool {
        self.narrow_outputs().any(|a| other.narrow_outputs().any(|b| Self::paths_overlap(a, b)))
    }

    pub(crate) fn rework(&self) -> bool {
        self.rework_since.is_some() && !self.status.finished() && !self.assignee.is_empty()
    }

    pub(crate) fn open_attempt(&self) -> Option<&Attempt> {
        self.attempts.iter().rev().find(|attempt| attempt.state.open())
    }
    pub(crate) fn open_attempt_mut(&mut self) -> Option<&mut Attempt> {
        self.attempts.iter_mut().rev().find(|attempt| attempt.state.open())
    }
    /// A check task is decided by the exit code of its command (C5).
    pub(crate) fn is_check(&self) -> bool {
        self.kind == TaskKind::Check
    }
}

/// Task #229: what a phase is. A plan phase belongs to the goal; a Minor phase
/// holds the Minor follow-ups the lead spawned, may run in free mode and may end
/// with tail tasks that depend on all its minors.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PhaseKind {
    #[default]
    Plan,
    Minor,
}

impl PhaseKind {
    pub(crate) fn is_plan(&self) -> bool {
        *self == Self::Plan
    }

    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Minor => "minor",
        }
    }
}

/// Task #289: a notice the lead marked seen, with the change signature it had
/// then; it returns only when that signature differs.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct SeenNotice {
    pub key: String,
    pub signature: String,
    #[serde(default)]
    pub at: f64,
}

/// What happened to a phase appended by `extend`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PhaseState {
    /// Waits for the owner's approval.
    #[default]
    Draft,
    /// The owner approved it, or its approval was `auto`. Task 158 (owner,
    /// 2026-11-07): an approved phase runs alongside the phases before it;
    /// its tasks wait only for their dependencies, for the outputs an open
    /// task holds (task 155), for a barrier and for an owner-acceptance gate.
    /// A record from before reads its old `queued` state as approved.
    #[serde(alias = "queued")]
    Approved,
    /// The owner discarded it; its ids stay used.
    Discarded,
    /// U25: the owner sent the draft back with a comment; the lead reworks it
    /// into a draft again. Its tasks are not handed out.
    Rejected,
    /// U63 (owner, 2026-10-31): a phase that requires the owner's acceptance
    /// has every task decided and waits for that acceptance; no later phase
    /// starts meanwhile.
    AwaitingAcceptance,
    /// U63: the owner accepted the phase. Only a phase that required the
    /// owner's acceptance reaches it; any other finished phase stays approved.
    Done,
}

impl PhaseState {
    /// A draft or a draft sent back: not accepted yet, still editable by the
    /// lead, numbered with the drafts.
    pub(crate) fn is_draft(self) -> bool {
        matches!(self, Self::Draft | Self::Rejected)
    }
    /// Accepted: running or finished (U63: or awaiting the owner's
    /// acceptance, or accepted by the owner).
    pub(crate) fn is_accepted(self) -> bool {
        matches!(self, Self::Approved | Self::AwaitingAcceptance | Self::Done)
    }
}

/// U63 (owner, 2026-10-31): who accepts a phase once its tasks are decided:
/// the lead's decisions on the tasks (as before), or the owner's acceptance
/// of the whole phase.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PhaseAcceptance {
    #[default]
    Lead,
    Owner,
}

impl PhaseAcceptance {
    pub(crate) fn is_lead(&self) -> bool {
        *self == Self::Lead
    }
}

/// U63 (owner, 2026-10-31): an accepted phase held before its work starts;
/// its tasks are not handed out until the pause is removed.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct PhasePause {
    /// The actor label: "You" or the lead's.
    pub by: String,
    /// The owner paused it, so only the owner removes the pause.
    #[serde(default)]
    pub owner: bool,
    #[serde(default)]
    pub at: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// U25: the owner's send-back of a draft phase.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct PhaseRejection {
    pub comment: String,
    #[serde(default)]
    pub by: String,
    #[serde(default)]
    pub at: f64,
    /// When the lead reworked the phase back into a draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reworked_at: Option<f64>,
}

/// One phase appended to the plan after the approved one. Phase 1 is implicit
/// and has no record.
/// Task 128: the team's lore writer state (docs/lore.md 6.1).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct LoreState {
    /// The hash of the records the last complete pass wrote: equal, nothing runs.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hash: String,
    /// Records that wait on a field only the lead or you can give.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<LorePending>,
    /// Refused records by id, with the content hash they were refused at:
    /// never retried until the record changes.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub refused: std::collections::BTreeMap<String, String>,
    /// The journal said once that the lore binary is missing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unavailable_told: bool,
    /// The last write, and the ids it wrote (newest first, at most 10).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub written_at: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent: Vec<String>,
}

impl LoreState {
    pub(crate) fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Task 128: a record Zerus cannot write until a field is known.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct LorePending {
    pub id: String,
    pub kind: String,
    pub missing: Vec<String>,
    pub text: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Phase {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<PhaseCheckResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<ArchivedPhase>,
    pub n: u32,
    /// Task #229: a plan phase (the default) or a Minor phase the lead opened
    /// for a set of Minor follow-ups; a Minor phase may run in free mode.
    #[serde(default, skip_serializing_if = "PhaseKind::is_plan")]
    pub kind: PhaseKind,
    /// The display and approval order (F10): drafts may be moved before each
    /// other. 0 on a team stored before the field means n order.
    #[serde(default)]
    pub order: u32,
    #[serde(default)]
    pub title: String,
    /// No task of this phase starts before every lower phase is finished.
    #[serde(default)]
    pub barrier: bool,
    /// The effective phase approval when the phase was added.
    #[serde(default)]
    pub approval: Approval,
    #[serde(default)]
    pub state: PhaseState,
    /// The phase's task ids; a discarded phase keeps them, so they are never reused.
    #[serde(default)]
    pub tasks: Vec<u32>,
    #[serde(default)]
    pub added_by: String,
    #[serde(default)]
    pub added_at: f64,
    #[serde(default)]
    pub decided_by: String,
    #[serde(default)]
    pub decided_at: Option<f64>,
    /// The owner's sentence when the lead approved this phase on its word (F13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_word: Option<OwnerWord>,
    /// The lead's edits to this draft since it was added, newest last, for the
    /// owner's diff view (F10): {"kind":"added|removed|changed|moved", ...}.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<serde_json::Value>,
    /// D2: an urgent phase runs at once (no approval under review, no barrier)
    /// and its tasks are handed out before other pending ones.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub urgent: bool,
    /// Section 9.5: why a "Rework" draft was opened after the parent lead
    /// rejected the sub-team's report.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// U25: the note shown under the phase header, such as "Reworked after
    /// your comment: ...".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// U25: the owner's latest send-back; kept after the rework for the note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection: Option<PhaseRejection>,
    /// U63 (owner, 2026-10-31): the pause that holds this accepted phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause: Option<PhasePause>,
    /// U63: whether the owner accepts this phase when its tasks are decided.
    #[serde(default, skip_serializing_if = "PhaseAcceptance::is_lead")]
    pub acceptance: PhaseAcceptance,
    /// U63: the owner sent the phase back at its acceptance; the lead reworks
    /// it in the same phase, and it waits for the acceptance again once the
    /// added work is decided.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub acceptance_rework: bool,
    /// U63: when the phase began to wait for the owner's acceptance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_since: Option<f64>,
}

/// Constant-size totals survive eviction of archived phase headers.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct ArchiveTotals {
    pub phases: usize,
    pub tasks: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub cancelled: usize,
}

impl ArchiveTotals {
    pub(crate) fn add(&mut self, phase: &ArchivedPhase) {
        self.phases += 1;
        self.tasks += phase.tasks;
        self.accepted += phase.accepted;
        self.rejected += phase.rejected;
        self.cancelled += phase.cancelled;
    }
    pub(crate) fn remove(&mut self, phase: &ArchivedPhase) {
        self.phases -= 1;
        self.tasks -= phase.tasks;
        self.accepted -= phase.accepted;
        self.rejected -= phase.rejected;
        self.cancelled -= phase.cancelled;
    }
}

/// Closed phase header; task ids and histories are paged from the archive.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct ArchivedPhase {
    pub tasks: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub cancelled: usize,
    pub closed_at: Option<f64>,
}

impl Phase {
    /// The display and approval order; a team stored before the field keeps
    /// its phase numbers as the order (F10).
    pub(crate) fn order(&self) -> u32 {
        if self.order == 0 {
            self.n
        } else {
            self.order
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Team {
    /// Highest archived task id; evicting stubs never frees an archived id.
    #[serde(default)]
    pub last_archived_task: u32,
    #[serde(default)]
    pub archive_totals: ArchiveTotals,
    /// Read-through dependency metadata; never grows the persisted live record.
    #[serde(skip)]
    pub archive_deps: std::collections::BTreeMap<u32, Task>,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub project: String,
    /// Stable logical identity; the existing project field remains its label.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub project_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub iron_rules_error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iron_rules_error_at: Option<f64>,
    /// Task 164 (owner, 2026-11-07): when the lead last got the team's starved
    /// digest; at most one an hour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starved_digest_at: Option<f64>,
    /// Task #464 (owner, 2026-11-09): the accepted tasks the landing merger was
    /// already woken for, newest last (bounded): a new accept wakes it again,
    /// an old one never does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub landing_told: Vec<u32>,
    #[serde(default)]
    pub folder: String,
    #[serde(default)]
    pub template: String,
    #[serde(default)]
    pub status: TeamStatus,
    /// U59: standard planning (phases planned, approved, run and closed) or the
    /// free mode, whose open phase never closes by itself. Standard is the
    /// default and every stored team deserializes as standard.
    #[serde(default, skip_serializing_if = "PlanMode::is_standard")]
    pub plan_mode: PlanMode,
    /// Task #58 (owner, 2026-11-06): the mode of each phase that has its own,
    /// by phase number (phase 1 included, which has no record). A phase not
    /// named here plans in the team's `plan_mode`, so a team stored before
    /// this field reads its mode on every phase, and a new phase starts in it.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub phase_modes: std::collections::BTreeMap<u32, PlanMode>,
    /// U62: the owner's one-time permission for one full test run, bound to a
    /// release; the next full run uses it once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_tests_grant: Option<FullTestsGrant>,
    /// U62 (#717): when the worker last read the machine's load for the test
    /// queue; a read is at least 30 s after the one before.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub test_load_at: f64,
    /// U62 (#717): the newest test results, one card each; the logs stay in the
    /// test database and this list is bounded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_notices: Vec<TestNotice>,
    /// #718: test items for the lead (flaky, time and size budgets), bounded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_alerts: Vec<TestAlert>,
    /// #718: flaky suites in quarantine, until they expire.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_quarantine: Vec<TestQuarantine>,
    /// #718: when the last full run was recorded; reuse counts from then.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub test_full_at: f64,
    /// #718: the open proposal of a full run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_full_proposal: Option<FullRunProposal>,
    /// U62 (#760): the newest lead-branch commit whose merged range the merge
    /// queue tested; a phase integration checks only what came after it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub merge_tested: String,
    /// Task 377 (owner, 2026-11-08): the line's branch by name, recorded
    /// when the integration folder was granted. The folder's current
    /// checkout is never the line.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub line_branch: String,
    /// U62 (#760): the newest merged commit each suite ran on. A suite this
    /// computer cannot run keeps its own range open without holding the tested
    /// point of the suites that did run. Bounded.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub merge_tested_suites: std::collections::BTreeMap<String, String>,
    /// U62 (#760): suites whose ranges are still open because no runner here
    /// could run them; kept separate from the bounded merge log.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub merge_not_run: std::collections::BTreeMap<String, OpenNotRun>,
    /// U62 (#760): the merges the queue kept, newest last (bounded).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merge_log: Vec<MergeLog>,
    /// U59: when the last integration-and-build close of a free phase passed;
    /// the desktop shows it with the free phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_build_at: Option<f64>,
    /// U59: the lead's proposed plan mode for the plan being made, and its
    /// one-line reason; the owner's approval chooses one of the two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_mode_offer: Option<PlanMode>,
    /// Task 127 (docs/lore.md 4.2): the memory mode the lead asked for on a
    /// `switch-memory-mode` card, and why; cleared when the owner decides.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub memory_mode_offer: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub memory_mode_reason: String,
    /// Task 159: the owner's scratch rule this team was created under (its
    /// id): no per-plan approval, its folder trusted for the rule's agents.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scratch_rule: String,
    /// Task 156: the owner's decisions, oldest first (at most 500): kept in
    /// Zerus state for every memory mode, mirrored into the team's lore.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<Decision>,
    /// Task 156: lore's `find` hits over decisions per task (a workspace team).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub decision_hits: std::collections::BTreeMap<u32, DecisionHits>,
    /// Task 156: the decisions the generated spec sections were last written for.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub decision_specs: String,
    /// Task 128: what the background writer last put into the team's lore
    /// workspace, and what waits on a field only a person can give.
    #[serde(default, skip_serializing_if = "LoreState::is_empty")]
    pub lore: LoreState,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub plan_mode_reason: String,
    /// U59: tasks accepted since the last build of the free phase, so the header
    /// says how much the next build covers.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub free_accepted_since_build: u64,
    /// U59: the free phase a pending close waits for; it closes when the build
    /// task is accepted and stays open if the build fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_close_pending: Option<u32>,
    #[serde(default)]
    pub settings: Settings,
    /// Task 270 (owner, 2026-11-08): the working-mode preset this project
    /// follows (branch-pipeline, branch-per-task or shared-folder); empty for a
    /// team created before the mode was proposed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub working_mode: String,
    /// Task 270: the project's working-mode proposal (preset, title, reasons,
    /// settings and profile), read once at planning or create and cached here;
    /// {"preset":""} means "checked, no repository to profile".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_proposal: Option<serde_json::Value>,
    /// Task 336 (owner, 2026-11-08): the owner kept the team's settings for the
    /// project's profile ({preset, fingerprint, at}); the working-mode block
    /// stays away until the profile changes materially.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_kept: Option<serde_json::Value>,
    /// Task 271 (owner, 2026-11-08): the lead playbook (the working-mode preset
    /// and the rules a-(i), src/state/teams/playbook.rs), stored with the
    /// team and carried in its lore by the lead brief.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playbook: Option<serde_json::Value>,
    /// Task 271: the lead-session fingerprint (run, conversation, phase) and
    /// reason of the last lead brief, so a compaction or a clear is briefed once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead_briefed: Option<serde_json::Value>,
    /// The member id of the lead.
    pub lead: String,
    #[serde(default)]
    pub members: Vec<Member>,
    #[serde(default)]
    #[serde(serialize_with = "serialize_tasks")]
    pub tasks: Vec<Task>,
    /// Phases added by `extend`; the approved initial plan (phase 1) has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde(serialize_with = "serialize_phases")]
    pub phases: Vec<Phase>,
    /// Permissions a plan widens beyond the role's default (B5): the owner
    /// confirms them before the plan can be approved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profile_widening: Vec<serde_json::Value>,
    /// D2: findings filed by accepted reports, oldest first (at most 1000).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
    /// D2: the last finding number given; ids stay unique for the team's life.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub findings_seq: u64,
    /// F5: the editing state while the team is a draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<Draft>,
    /// F7: the team this one continues, and the team that continues it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub previous: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub next: String,
    /// N7: why a stopped team stopped ("draft_discarded"), empty otherwise.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stop_reason: String,
    /// F8: membership and next-goal proposals the lead made.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proposals: Vec<serde_json::Value>,
    #[serde(default)]
    pub revision: u64,
    /// F11: the revision that last changed draft phase or task numbers; a
    /// caller holding an older revision is refused.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub phase_numbers_revision: u64,
    #[serde(default)]
    pub created: f64,
    #[serde(default)]
    pub updated: f64,
    /// A team created from the desktop starts its lead and sends it this
    /// planning brief once, like a report to the lead.
    #[serde(default)]
    pub brief: Notice,
    #[serde(default)]
    pub brief_id: String,
    /// The "all tasks decided" message to the lead: sent once per such state.
    #[serde(default, skip_serializing_if = "Notice::is_none")]
    pub idle: Notice,
    /// The lead's coalesced inbox message (C2).
    #[serde(default, skip_serializing_if = "Inbox::is_empty")]
    pub inbox: Inbox,
    /// Who approved the plan: "You", or "Lead" on the owner's word (F13).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub approved_by: String,
    /// U37: when the plan was approved: phase 1 opens here. Absent on an old
    /// team.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<f64>,
    /// The owner's sentence the lead approved the plan on (F13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_word: Option<OwnerWord>,
    /// Who finished the team and with what summary; set by `finish`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<Finish>,
    /// The template's lead protocol when the team was created, given to the
    /// lead with the planning brief.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub lead_protocol: String,
    /// H6: the sessions members were swapped away from, oldest first, at most
    /// 20: {member, old, at, by}. The old session keeps running for the owner.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub swapped: Vec<serde_json::Value>,
    /// Task 135: the handoff briefs, oldest first (at most 20 versions per task
    /// and 200 in all; see `handoff::record`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handoffs: Vec<Handoff>,
    /// Section 9.1: the parent team and task this sub-team was delegated
    /// from; absent on a master (or any flat) team.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Parent>,
    /// Section 9.1: the sub-teams delegated from this team, at most 32.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<String>,
    /// Task #228: the parent team and creating task of a QA member's scratch
    /// team; absent on every other team.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scratch_parent: Option<ScratchParent>,
    /// Task #228: the scratch teams this team's members created, newest last,
    /// at most 32; its lead reads created and stopped in the feed and inbox.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scratch_teams: Vec<ScratchChild>,
    /// U26: verified owner events already spent on a change, so one sentence
    /// authorizes one change: [{event, change}].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub used_owner_words: Vec<serde_json::Value>,
    /// U29: additions to running phases waiting for the owner's confirmation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_additions: Vec<PendingAddition>,
    /// Task 162: the lead's changes of the plan inside the approved goal
    /// (tasks added, moved, split or edited in accepted phases), each with
    /// what it replaced, so the owner sees them in a digest and can revert
    /// any; newest last, at most `LEAD_CHANGES_KEPT`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lead_changes: Vec<LeadChange>,
    /// Task #289: the inbox notices the lead marked seen, oldest first, at most
    /// 300; each returns to the inbox when its content changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seen_notices: Vec<SeenNotice>,
    /// #511 N3: members to tell that their worked-ahead attempt was revoked
    /// with its phase, newest last, at most 20. Task 24: also a member whose
    /// attempt in progress was revoked by a reassign, a take-over or a cancel
    /// (phase 0).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stops: Vec<StopNotice>,
    /// U51: the owner's intake keys; the key itself is never stored, only its
    /// hash, and the key is shown once when it is created.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intakes: Vec<Intake>,
    /// U51: the last intake key number given; ids stay unique after a
    /// revocation.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub intakes_seq: u64,
    /// Task 95: the lead's decisions wait past `lead_stall_minutes` while its
    /// session does not work on them; the owner is told, again at most
    /// hourly, until the lead moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead_stall: Option<LeadStall>,
    /// Task 112 (owner, 2026-11-06: "I paused it in the morning, unpaused it
    /// in the afternoon, but Zerus still thinks it is paused"): when the team
    /// was paused, by whom and why; only while it is paused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<TeamPause>,
    /// Task #227: a retry on a task its member is working on does not revoke
    /// it; the notice to continue reaches the member at its turn's safe point,
    /// newest last, at most 20.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub continues: Vec<ContinueNotice>,
    /// Task #227: the attempts a restart gave back by itself (re-sent, or
    /// bound to the work already running), newest last, at most 20; the lead
    /// reads them as one inbox item.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub crash_recovered: Vec<Value>,
    /// Task #275: a sandboxed member's queued call the worker refused or that
    /// failed, newest last, at most 20; the member is told as a team message
    /// and the lead reads each reason in its inbox.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outbox_failures: Vec<Value>,
}

/// Task #227: a retry while the member works on the task: "continue", never
/// "start over". The notice is a team message queued for the member's turn end.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct ContinueNotice {
    pub id: String,
    pub task: u32,
    pub attempt: u32,
    pub member: String,
    #[serde(default)]
    pub by: String,
    pub at: f64,
    #[serde(default)]
    pub notice: Notice,
    /// A refused send is tried on later ticks, ten times at most.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub tries: u32,
}

/// Task 112: the pause of a team, as the team bar, Waiting for you and the
/// lead's inbox name it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct TeamPause {
    pub at: f64,
    /// "You", "Lead" or another actor's label.
    #[serde(default)]
    pub by: String,
    /// "paused", or "replan" when a replan paused it.
    #[serde(default)]
    pub reason: String,
}

/// Task 95 (owner, 2026-11-06): the lead stopped working while decisions wait
/// for it: one owner-level item, notified once per `round`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct LeadStall {
    /// When the dispatcher raised it.
    pub since: f64,
    /// What the lead waits on: `question` (its own question or permission),
    /// `rejected_tool` (a refused tool call ended its turn), `exited` (its
    /// agent stopped) or `idle`.
    pub reason: String,
    /// The lead's own question, when `reason` is `question`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// The decisions waiting past the threshold, and the members they block.
    pub items: u32,
    pub members: u32,
    /// The oldest of those decisions.
    pub oldest: f64,
    /// The notification number: 1 when raised, one more each repeat.
    pub round: u32,
    /// When the owner was last notified (the round started).
    pub notified_at: f64,
    /// Task 139: the idle lead itself was told this round.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lead_told: bool,
}

impl LeadStall {
    /// At most one notification per this many seconds.
    pub(crate) const REPEAT_SECONDS: f64 = 3600.0;

    /// The sentence that names what the lead waits on.
    pub(crate) fn waits_on(&self) -> String {
        match self.reason.as_str() {
            "question" if !self.detail.is_empty() => format!("its own question: {}", self.detail),
            "question" => "its own question or permission".into(),
            "rejected_tool" => "a refused tool call ended its turn".into(),
            "exited" => "its agent exited".into(),
            _ => "it is idle".into(),
        }
    }

    /// The owner-facing title: what the lead waits on and what it blocks.
    pub(crate) fn title(&self, now: f64) -> String {
        let minutes = ((now - self.oldest).max(0.0) / 60.0).round() as u64;
        let items = if self.items == 1 { "1 decision".to_owned() } else { format!("{} decisions", self.items) };
        let members = match self.members {
            0 => String::new(),
            1 => ", 1 member blocked".to_owned(),
            n => format!(", {n} members blocked"),
        };
        format!("The lead stalled ({}): {items} wait{} for it, the oldest {minutes} min{members}",
            self.waits_on(), if self.items == 1 { "s" } else { "" })
    }
}

/// #511 N3: a revoked worked-ahead attempt whose member must stop. The
/// dispatcher sends it once under `id`; an uncertain send is never repeated.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct StopNotice {
    /// The send's request id.
    pub id: String,
    pub task: u32,
    #[serde(default)]
    pub title: String,
    pub member: String,
    /// The revoked attempt's number.
    pub attempt: u32,
    #[serde(default)]
    pub phase: u32,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub at: f64,
    #[serde(default)]
    pub notice: Notice,
    /// Refused sends so far; after ten it is left unconfirmed for the lead.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub refused: u32,
    /// Task 24: the owner's interrupt_revoked held when the attempt was
    /// revoked: a busy session is interrupted once before the notice.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interrupt: bool,
    /// Task 24: that interrupt was asked for; it is never repeated.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
}

impl Team {
    /// Task 174: the folder whose branch integrates the team's work (the lead
    /// branch): the owner's `integration_folder`, else the team's folder.
    pub(crate) fn integration_folder(&self) -> &str {
        if self.settings.integration_folder.trim().is_empty() {
            &self.folder
        } else {
            self.settings.integration_folder.trim()
        }
    }

    /// Task #58: the mode phase `n` plans in: its own, else the team's.
    pub(crate) fn phase_mode(&self, n: u32) -> PlanMode {
        self.phase_modes.get(&n).copied().unwrap_or(self.plan_mode)
    }

    /// U26: the change an already-spent owner word paid for, if any.
    pub(crate) fn owner_word_used(&self, event: i64) -> Option<String> {
        self.used_owner_words
            .iter()
            .find(|used| used["event"].as_i64() == Some(event))
            .map(|used| used["change"].as_str().unwrap_or_default().to_owned())
    }

    /// U32/N7: how many times that word has paid for a change so far (a stored
    /// word without the count paid once).
    #[cfg(test)]
    pub(crate) fn owner_word_uses(&self, event: i64) -> usize {
        self.used_owner_words
            .iter()
            .find(|used| used["event"].as_i64() == Some(event))
            .map(|used| used["uses"].as_u64().unwrap_or(1) as usize)
            .unwrap_or(0)
    }

    /// U32/N7: whether the next use of that word is still the change it says:
    /// a count word ("add two backends") authorizes that many adds, every other
    /// word one change.
    pub(crate) fn owner_word_allowed_again(&self, event: i64, allowed: usize, change: &str) -> bool {
        let spent: Vec<&serde_json::Value> = self.used_owner_words.iter().filter(|used| used["event"].as_i64() == Some(event)).collect();
        if spent.is_empty() {
            return true;
        }
        if let Some(same) = spent.iter().find(|used| used["change"].as_str() == Some(change)) {
            return (same["uses"].as_u64().unwrap_or(1) as usize) < allowed.max(1);
        }
        // Task #21 (owner, 2026-11-05): "relaunch hgs, relaunch ui2" pays once
        // for each member it names (the name check ran on the whole message);
        // a word never pays for a change of another kind.
        member_change(change) && spent.iter().all(|used| used["change"].as_str().is_some_and(member_change))
    }

    /// U58: one sentence naming a member and its tasks pays for both changes,
    /// the member first: the roster add spent once, then its tasks' phase
    /// request. A word used for anything else, or already twice, never does.
    pub(crate) fn owner_word_allows_its_tasks(&self, event: i64) -> bool {
        self.used_owner_words.iter().any(|used| {
            used["event"].as_i64() == Some(event)
                && used["change"].as_str().is_some_and(|change| change.starts_with("adding "))
                && used["uses"].as_u64().unwrap_or(1) == 1
        })
    }

    pub(crate) fn record_owner_word(&mut self, event: i64, change: &str) {
        // Task #21: a word that paid for several members keeps one record each.
        let per_member = member_change(change);
        if let Some(used) = self
            .used_owner_words
            .iter_mut()
            .find(|used| used["event"].as_i64() == Some(event) && (!per_member || used["change"].as_str() == Some(change)))
        {
            used["uses"] = serde_json::json!(used["uses"].as_u64().unwrap_or(1) + 1);
        } else {
            self.used_owner_words.push(serde_json::json!({"event":event,"change":change,"uses":1}));
            let excess = self.used_owner_words.len().saturating_sub(50);
            self.used_owner_words.drain(..excess);
        }
    }
    /// Owner feedback 2026-11-04 (4): `max_members` counts the members
    /// besides the lead, so "six members" means six beside their lead.
    pub(crate) fn capped_members(&self) -> usize {
        self.members.iter().filter(|member| member.state != MemberState::Removed && member.id != self.lead).count()
    }
    pub(crate) fn member(&self, id: &str) -> Option<&Member> {
        self.members.iter().find(|member| member.id == id)
    }
    pub(crate) fn task(&self, id: u32) -> Option<&Task> {
        self.tasks.iter().find(|task| task.id == id).or_else(|| self.archive_deps.get(&id))
    }
    pub(crate) fn task_mut(&mut self, id: u32) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|task| task.id == id)
    }
    /// The role name a hand-off or report header shows for a member.
    pub(crate) fn role_of(&self, member: &str) -> String {
        if member == self.lead {
            return "Lead".into();
        }
        self.member(member).map(|m| m.role.clone()).unwrap_or_else(|| member.to_owned())
    }

    /// U56: how a notice names a member, "b-body (backend-opus)": its id with
    /// its role, since several members may share a role. The lead whose id is
    /// "lead" is "Lead"; an unknown or roleless member is its id.
    pub(crate) fn name_of(&self, member: &str) -> String {
        let role = self.role_of(member);
        if member.is_empty() || role.is_empty() || role == member {
            return if member.is_empty() { role } else { member.to_owned() };
        }
        if member == self.lead && member.eq_ignore_ascii_case("lead") {
            return role;
        }
        format!("{member} ({role})")
    }

    /// An explicit assignment policy, independent of other tasks' Output.
    pub(crate) fn requires_assignee(&self, task: &Task) -> bool {
        task.rework() || task.pinned || (self.settings.keep_assignees && task.role.is_empty() && !task.assignee.is_empty())
    }

    /// Assignment policy only. Output protection applies separately to
    /// busy-assignee rebalance, never to choosing a fresh retry member.
    pub(crate) fn keeps_assignee(&self, task: &Task) -> bool {
        self.requires_assignee(task)
    }

    /// Recheck at the mutation boundary. Tick selection uses one shared index.
    pub(crate) fn rebalance_blocked(&self, task: &Task) -> bool {
        self.requires_assignee(task) || (!task.requests_fresh_retry() && InProgressOutputs::new(self).blocks(task))
    }

    /// A returned task reserves its member even while held or awaiting review.
    pub(crate) fn reserved_for_rework(&self, member: &str, task: Option<u32>) -> bool {
        // Task 160 (owner, 2026-11-07): a held reject or a parked rework at
        // the head of a member's queue never blocks its later hand-outs.
        self.tasks.iter().filter(|found| found.rework() && found.assignee == member && found.hold.is_none() && found.park.is_none())
            .min_by(|a, b| a.rework_since.unwrap().total_cmp(&b.rework_since.unwrap()).then(a.id.cmp(&b.id)))
            .is_some_and(|found| Some(found.id) != task)
    }

    /// Spec 5.6: who reviews a member's work - its parent when that is a
    /// sub-lead still in the team, else the lead. The lead has no reviewer.
    pub(crate) fn reviewer_of(&self, member: &str) -> String {
        if member == self.lead {
            return String::new();
        }
        let parent = self.member(member).map(|m| m.parent.as_str()).unwrap_or_default();
        match self.member(parent) {
            Some(found) if found.sub && found.state != MemberState::Removed && found.id != member && found.id != self.lead => found.id.clone(),
            _ => self.lead.clone(),
        }
    }

    /// A7: the lowest task whose undecided report Zerus captured from the
    /// member's last reply (A2). A captured reply may be only an
    /// acknowledgement ("queued, I'll do it after X"), so the member gets no new
    /// task, rebalance or work ahead until its reviewer decides that task or
    /// the member replaces the report with its own (A6). The lead's own tasks
    /// never hold it (C1).
    pub(crate) fn captured_hold(&self, member: &str) -> Option<u32> {
        if member == self.lead {
            return None;
        }
        self.tasks
            .iter()
            // #538 C1: a captured report holds its member at any stage too.
            .filter(|task| task.status.reported())
            .filter(|task| {
                task.attempts.iter().rev().find(|attempt| attempt.state == AttemptState::Reported).is_some_and(|attempt| {
                    attempt.member == member && attempt.report.as_ref().is_some_and(|report| report.captured)
                })
            })
            .map(|task| task.id)
            .min()
    }

    /// The members that report directly to `member` (spec 5.6), in roster
    /// order; the lead's are those without a sub-lead parent.
    pub(crate) fn reports_of(&self, member: &str) -> Vec<&Member> {
        self.members
            .iter()
            .filter(|m| m.id != self.lead && m.state != MemberState::Removed && self.reviewer_of(&m.id) == member)
            .collect()
    }
}

/// Archived rows have a deliberately small wire representation: what the plan
/// still draws of a done phase. Its dependencies (task #469) and the task it
/// was spawned from are among them, or the phase falls into one layer without
/// links; its history stays in the archive's payload.
pub(crate) fn task_stub(task: &Task) -> serde_json::Value {
    let mut stub = serde_json::json!({"id":task.id,"phase":task.phase,"title":task.title,"assignee":task.assignee,
        "status":task.status,"archived":task.archived});
    if !task.deps.is_empty() { stub["deps"] = serde_json::to_value(&task.deps).unwrap_or_default(); }
    if let Some(origin) = task.origin { stub["origin"] = serde_json::json!(origin); }
    stub
}

pub(crate) fn phase_stub(phase: &Phase) -> serde_json::Value {
    serde_json::json!({"n":phase.n,"order":phase.order,"title":phase.title,
        "state":phase.state,"acceptance":phase.acceptance,"decided_at":phase.decided_at,"archived":phase.archived})
}

fn serialize_phases<S: serde::Serializer>(phases: &[Phase], serializer: S) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let mut seq = serializer.serialize_seq(Some(phases.len()))?;
    for phase in phases {
        if phase.archived.is_some() {
            seq.serialize_element(&phase_stub(phase))?;
        } else { seq.serialize_element(phase)?; }
    }
    seq.end()
}

fn serialize_tasks<S: serde::Serializer>(tasks: &[Task], serializer: S) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let mut seq = serializer.serialize_seq(Some(tasks.len()))?;
    for task in tasks {
        if task.archived.is_some() { seq.serialize_element(&task_stub(task))?; }
        else { seq.serialize_element(task)?; }
    }
    seq.end()
}

/// An estimate source of "none" is the default and is not stored.
fn no_estimate_source(source: &str) -> bool {
    source.is_empty() || source == "none"
}

/// Task #21: a roster change about one named member ("relaunching hgs"); one
/// owner message naming several members pays once for each of them.
fn member_change(change: &str) -> bool {
    ["relaunching ", "removing ", "draining "].iter().any(|prefix| change.strip_prefix(prefix).is_some_and(|rest| !rest.is_empty() && rest != "the members"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn old_team() -> Value {
        json!({
            "id":"old",
            "name":"Old team",
            "lead":"lead",
            "status":"running",
            "settings":{"approval":"auto"},
            "members":[{"id":"lead","role":"Lead","agent":"claude"}],
            "tasks":[{"id":1,"title":"Model","status":"accepted"},{"id":2,"title":"Tests","deps":[{"task":1}]}]
        })
    }

    #[test]
    fn old_team_json_loads_unchanged() {
        let team: Team = serde_json::from_value(old_team()).unwrap();
        assert!(team.phases.is_empty());
        // Absent phase approval means the plan's approval.
        assert_eq!(team.settings.phase_approval, None);
        assert_eq!(team.settings.phase_approval.unwrap_or(team.settings.approval), Approval::Auto);
        assert!(team.tasks.iter().all(|task| task.phase == 1 && !task.pinned));
        // Writing it back adds only the phase to each task.
        let value = serde_json::to_value(&team).unwrap();
        assert!(value.get("phases").is_none());
        assert!(value["settings"].get("phase_approval").is_none());
        assert!(value["tasks"].as_array().unwrap().iter().all(|task| task["phase"] == json!(1) && task.get("pinned").is_none()));
        assert_eq!(value["settings"]["approval"], "auto");
        assert_eq!(value["tasks"][1]["deps"][0], json!({"task":1,"kind":"hard"}));
        // H1/H6: the new optional fields stay out of an old team's file.
        assert!(value["settings"].get("resume_after_limits").is_none());
        assert!(value.get("swapped").is_none());
        // Section 9: a flat team has no parent, no children and the default
        // tree settings are not written.
        assert!(value.get("parent").is_none() && value.get("children").is_none());
        assert_eq!((team.settings.tree_depth, team.settings.tree_sessions), (2, 16));
        assert_eq!(team.settings.subteam_approval, SubteamApproval::Parent);
        assert!(value["settings"].get("tree_depth").is_none() && value["settings"].get("tree_sessions").is_none());
        assert!(value["settings"].get("subteam_approval").is_none());
        let back: Team = serde_json::from_value(value).unwrap();
        assert_eq!(back, team);
    }

    #[test]
    fn a_subteam_link_and_children_round_trip() {
        let mut team: Team = serde_json::from_value(old_team()).unwrap();
        team.parent = Some(Parent { team: "master".into(), task: 3, lead: "lead".into(), member: "dev".into() });
        team.children = vec!["sub1".into(), "sub2".into()];
        team.tasks[0].subteam = Some("sub1".into());
        let value = serde_json::to_value(&team).unwrap();
        assert_eq!(value["parent"]["team"], "master");
        assert_eq!(value["parent"]["task"], 3);
        assert_eq!(value["children"], json!(["sub1","sub2"]));
        assert_eq!(value["tasks"][0]["subteam"], "sub1");
        let back: Team = serde_json::from_value(value).unwrap();
        assert_eq!(back, team);
        // Settings beyond the defaults are written, and a changed approval.
        team.settings.tree_depth = 3;
        team.settings.tree_sessions = 24;
        team.settings.subteam_approval = SubteamApproval::Owner;
        let value = serde_json::to_value(&team).unwrap();
        assert_eq!(value["settings"]["tree_depth"], 3);
        assert_eq!(value["settings"]["tree_sessions"], 24);
        assert_eq!(value["settings"]["subteam_approval"], "owner");
    }

    #[test]
    fn the_limit_setting_and_member_episode_round_trip() {
        let mut team: Team = serde_json::from_value(old_team()).unwrap();
        assert!(!team.settings.resume_after_limits);
        team.settings.resume_after_limits = true;
        team.members[0].limited = Some(MemberLimit {
            at: 100.0,
            resets_at: Some(200.0),
            detail: "You've hit your session limit".into(),
        });
        team.members[0].limit_resume = true;
        let value = serde_json::to_value(&team).unwrap();
        assert_eq!(value["settings"]["resume_after_limits"], true);
        assert_eq!(value["members"][0]["limited"]["resets_at"], 200.0);
        let back: Team = serde_json::from_value(value).unwrap();
        assert!(back.settings.resume_after_limits && back.members[0].limit_resume);
        assert_eq!(back.members[0].limited.as_ref().and_then(|limit| limit.resets_at), Some(200.0));
        // An unknown reset time is optional.
        let limit: MemberLimit = serde_json::from_value(json!({"at":10.0})).unwrap();
        assert_eq!((limit.resets_at, limit.detail.as_str()), (None, ""));
        // H6: a task's continuation is optional.
        assert!(team.tasks.iter().all(|task| task.continuation.is_none()));
    }

    fn phased_team() -> Team {
        Team {
            id: "phased".into(),
            name: "Phased".into(),
            lead: "lead".into(),
            settings: Settings { approval: Approval::Review, phase_approval: Some(Approval::Auto), ..Default::default() },
            tasks: vec![Task { id: 3, phase: 2, title: "Fix".into(), pinned: true, ..Default::default() }],
            phases: vec![Phase {
                n: 2,
                title: "QA fixes".into(),
                barrier: true,
                approval: Approval::Auto,
                state: PhaseState::Draft,
                tasks: vec![3],
                added_by: "Lead".into(),
                added_at: 1792868401.5,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn new_phase_fields_round_trip() {
        let team = phased_team();
        let text = serde_json::to_string(&team).unwrap();
        let back: Team = serde_json::from_str(&text).unwrap();
        assert_eq!(back, team);
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["tasks"][0]["phase"], json!(2));
        assert_eq!(value["tasks"][0]["pinned"], json!(true));
        assert_eq!(value["phases"][0]["n"], json!(2));
        assert_eq!(value["phases"][0]["state"], json!("draft"));
        assert_eq!(value["phases"][0]["approval"], json!("auto"));
        assert_eq!(value["phases"][0]["barrier"], json!(true));
        assert_eq!(value["phases"][0]["decided_at"], Value::Null);
        assert_eq!(value["settings"]["phase_approval"], json!("auto"));
    }

    #[test]
    fn the_waiting_and_finish_fields_round_trip() {
        let mut team = phased_team();
        team.idle = Notice::Pending;
        team.finish = Some(Finish { by: "Lead".into(), summary: "Shipped it".into(), at: 1.5 });
        let text = serde_json::to_string(&team).unwrap();
        let back: Team = serde_json::from_str(&text).unwrap();
        assert_eq!(back, team);
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["idle"], "pending");
        assert_eq!(value["finish"]["summary"], "Shipped it");
        // Without them, the old record still has neither key.
        let old: Value = serde_json::to_value(serde_json::from_value::<Team>(old_team()).unwrap()).unwrap();
        assert!(old.get("idle").is_none() && old.get("finish").is_none());
    }

    #[test]
    fn phase_approval_falls_back_to_the_plan_approval() {
        let review = Settings::default();
        assert_eq!(review.phase_approval, None);
        assert_eq!(review.phase_approval.unwrap_or(review.approval), Approval::Review);
        let auto = Settings { approval: Approval::Auto, ..Default::default() };
        assert_eq!(auto.phase_approval.unwrap_or(auto.approval), Approval::Auto);
        let review_phases = Settings { approval: Approval::Auto, phase_approval: Some(Approval::Review), ..Default::default() };
        assert_eq!(review_phases.phase_approval.unwrap_or(review_phases.approval), Approval::Review);
    }

    /// An old attempt and report: no start, question, hold or capture fields.
    fn old_attempt_team() -> Value {
        json!({
            "id":"old","name":"Old team","lead":"lead","status":"running",
            "members":[{"id":"lead","role":"Lead","agent":"claude"},{"id":"dev","role":"Backend","agent":"codex"}],
            "tasks":[{"id":1,"title":"API","assignee":"dev","status":"submitted",
                "attempts":[{"n":1,"id":"00000000-0000-4000-8000-000000000001","member":"dev",
                    "state":"reported","sent_at":10.0,"delivered_at":11.0,"turn_ended_at":20.0,
                    "report":{"text":"Done","at":30.0}}]}]
        })
    }

    #[test]
    fn an_old_attempt_loads_with_the_new_fields_at_their_defaults() {
        let team: Team = serde_json::from_value(old_attempt_team()).unwrap();
        assert_eq!(team.settings.start_timeout_seconds, 180);
        // The lead's load settings (C2, C4, C12, U18) default; an old team has
        // no inbox.
        assert_eq!(
            (
                team.settings.inbox_interval_seconds,
                team.settings.remind_lead_minutes,
                team.settings.remind_owner_minutes,
                team.settings.remind_owner_permission_minutes
            ),
            (120, 15, 60, 10)
        );
        // Task 95: an old team hears of a stalled lead after the default.
        assert_eq!(team.settings.lead_stall_minutes, 20);
        // Task 97: an old team defers its sessions' prompts after the default.
        assert_eq!(team.settings.defer_minutes, 10);
        assert!(team.lead_stall.is_none());
        assert_eq!(team.settings.background_red_minutes, 30);
        assert!(team.inbox.told.is_empty() && team.inbox.told_at == 0.0);
        assert!(team.members.iter().all(|member| member.pending.is_none()));
        assert!(team.approved_by.is_empty() && team.owner_word.is_none() && team.approved_at.is_none());
        let attempt = &team.tasks[0].attempts[0];
        assert_eq!((attempt.started_at, attempt.start_warned), (None, false));
        assert!(attempt.reminded_at.is_none());
        assert!(attempt.check.is_none());
        assert!(attempt.turn_reply.is_empty());
        assert!(attempt.questions.is_empty());
        assert_eq!(attempt.open_question(), None);
        assert_eq!(team.tasks[0].hold, None);
        // An ordinary plan task keeps the old shape (C5: no kind, no check).
        assert_eq!(team.tasks[0].kind, TaskKind::Task);
        assert!(team.tasks[0].check.is_none());
        let report = attempt.report.as_ref().unwrap();
        assert!(!report.captured);
        assert!(report.replaced.is_empty());
        // Writing it back adds only the default timeouts to the settings; the
        // new fields stay absent so an old binary reads the file.
        let value = serde_json::to_value(&team).unwrap();
        assert_eq!(value["settings"]["start_timeout_seconds"], json!(180));
        assert_eq!(value["settings"]["background_red_minutes"], json!(30));
        assert!(value.get("inbox").is_none() && value.get("pending").is_none());
        assert!(value.get("approved_by").is_none() && value.get("owner_word").is_none());
        assert!(value["tasks"][0].get("kind").is_none() && value["tasks"][0].get("check").is_none());
        let attempt = &value["tasks"][0]["attempts"][0];
        assert!(value.get("approved_at").is_none());
        for key in ["started_at", "start_warned", "turn_reply", "questions", "reminded_at", "decided_at"] {
            assert!(attempt.get(key).is_none(), "{key} should be skipped when empty");
        }
        assert!(value["members"][1].get("pending").is_none());
        assert!(value["tasks"][0].get("hold").is_none());
        let report = &attempt["report"];
        assert!(report.get("captured").is_none() && report.get("replaced").is_none());
        let back: Team = serde_json::from_value(value).unwrap();
        assert_eq!(back, team);
    }

    fn held_team() -> Team {
        Team {
            id: "held".into(),
            name: "Held".into(),
            lead: "lead".into(),
            status: TeamStatus::Running,
            settings: Settings { start_timeout_seconds: 300, ..Default::default() },
            tasks: vec![Task {
                id: 7,
                title: "API".into(),
                assignee: "dev".into(),
                status: TaskStatus::Pending,
                hold: Some(Hold { by: "Lead".into(), at: 1792940000.5, reason: "Busy with #9".into() }),
                attempts: vec![Attempt {
                    n: 1,
                    id: "00000000-0000-4000-8000-000000000007".into(),
                    member: "dev".into(),
                    state: AttemptState::Rejected,
                    sent_at: 1792939000.0,
                    delivered_at: Some(1792939001.0),
                    started_at: Some(1792939010.0),
                    start_warned: true,
                    turn_ended_at: Some(1792939500.0),
                    turn_reply: "r7".into(),
                    questions: vec![
                        Question { id: "q1".into(), text: "Keep the old field?".into(), at: 1792939100.0, answer: "Yes".into(), answered_at: Some(1792939200.0), by: "Lead".into(), ..Default::default() },
                        Question { id: "q2".into(), text: "Which JSON?".into(), at: 1792939300.0, ..Default::default() },
                    ],
                    report: Some(Report {
                        text: "Done".into(),
                        at: 1792939600.0,
                        captured: true,
                        replaced: vec![ReplacedReport { text: "Building and running...".into(), at: 1792939550.0 }],
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn the_attempt_and_hold_fields_round_trip() {
        let team = held_team();
        let text = serde_json::to_string(&team).unwrap();
        let back: Team = serde_json::from_str(&text).unwrap();
        assert_eq!(back, team);
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["settings"]["start_timeout_seconds"], json!(300));
        let task = &value["tasks"][0];
        assert_eq!(task["hold"], json!({"by":"Lead","at":1792940000.5,"reason":"Busy with #9"}));
        let attempt = &task["attempts"][0];
        assert_eq!(attempt["started_at"], json!(1792939010.0));
        assert_eq!(attempt["start_warned"], json!(true));
        assert_eq!(attempt["turn_reply"], json!("r7"));
        // Every field of a question has its own name, answers stay visible.
        assert_eq!(attempt["questions"][0], json!({
            "id":"q1","text":"Keep the old field?","at":1792939100.0,"answer":"Yes",
            "answered_at":1792939200.0,"by":"Lead"}));
        assert_eq!(attempt["questions"][1]["answered_at"], Value::Null);
        assert_eq!(attempt["questions"][1]["answer"], "");
        let report = &attempt["report"];
        assert_eq!(report["captured"], json!(true));
        assert_eq!(report["replaced"], json!([{"text":"Building and running...","at":1792939550.0}]));
        // The open question is the unanswered one.
        let attempt = &back.tasks[0].attempts[0];
        assert_eq!(attempt.open_question().map(|q| q.id.as_str()), Some("q2"));
        assert!(!attempt.questions[0].open());
    }
}



