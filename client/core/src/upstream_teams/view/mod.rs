//! Compact view models of the Teams reads and their projections. Each reply is
//! projected once in the core and serialized as it is to the webview, which
//! never parses an `hgs` reply. Texts and lists are bounded; a view that left
//! something out says so (`truncated`) and names the next page's cursor
//! (`next`).
//!
//! Adapted from the fork's `client/core/src/teams/view.rs` (WhiteWh/zerus at
//! `ca4cc4481`), reduced to the views of M1 and to the reply shapes package 5
//! pins: the team list ([`list`]), one team (its bar, its members and its
//! queue as the board's groups in [`board`]) and the open owner cards
//! ([`waiting`]).
pub mod board;
mod editing;
pub mod graph;
pub use editing::MemberRole;
pub mod list;
pub mod phase_metrics;
pub mod phases;
pub mod planning;

pub use board::team;
pub use list::{list, waiting};

use serde::Serialize;
use serde_json::Value;

/// The longest name, title or short field kept.
pub const NAME_CHARS: usize = 200;
/// The longest free text kept.
pub const TEXT_CHARS: usize = 8000;
/// The longest line kept (a task title, a waiting text).
pub const LINE_CHARS: usize = 600;
/// Rows one page read asks for and one view keeps: the store's page bound.
pub const PAGE: usize = 200;
/// Teams one list keeps.
pub const LIST_KEPT: usize = PAGE;
/// Members one team keeps.
pub const MEMBERS_KEPT: usize = 256;
/// Stages one card keeps.
pub const STAGES_KEPT: usize = 16;
/// The board's groups, in their order; every board carries them all so the
/// view never reshuffles its columns.
pub const GROUPS: [&str; 7] = [
    "draft",
    "waiting",
    "pending",
    "in_progress",
    "reported",
    "accepted",
    "cancelled",
];

// ---------------------------------------------------------------- helpers

pub(crate) fn cut(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((at, _)) => text[..at].to_owned(),
        None => text.to_owned(),
    }
}

pub(crate) fn text(value: &Value, key: &str, chars: usize) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|text| cut(text, chars))
        .unwrap_or_default()
}

pub(crate) fn name(value: &Value, key: &str) -> String {
    text(value, key, NAME_CHARS)
}

/// A string field whose absence means the type's default (`not_created` for a
/// member, `draft` for a node): the CLI leaves defaults out.
pub(crate) fn or_default(value: &Value, key: &str, default: &str) -> String {
    match value.get(key).and_then(Value::as_str) {
        Some(found) if !found.is_empty() => cut(found, NAME_CHARS),
        _ => default.to_owned(),
    }
}

pub(crate) fn count(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(|found| {
            found
                .as_u64()
                .or_else(|| found.as_f64().map(|n| n.max(0.0) as u64))
        })
        .unwrap_or(0)
}

/// The most service notes one card carries, and the longest code and text
/// kept. A note is the service's own statement about a report, never the
/// member's words.
pub const NOTES_KEPT: usize = 8;
const NOTE_CODE_CHARS: usize = 32;
const NOTE_TEXT_CHARS: usize = 600;

/// One service note beside a quoted report: a short lower-case code and an
/// optional bounded text. The webview maps the code to its own words.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ServiceNote {
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<String>,
}

/// A row's `notes`: entries with a short lower-case code, at most
/// [`NOTES_KEPT`], each code once; anything else is ignored, never an error.
pub(crate) fn service_notes(row: &Value) -> Vec<ServiceNote> {
    let token = |code: &str| {
        !code.is_empty()
            && code.len() <= NOTE_CODE_CHARS
            && code.starts_with(|c: char| c.is_ascii_lowercase())
            && code
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    let mut kept: Vec<ServiceNote> = Vec::new();
    for entry in array(row, "notes").iter().take(NOTES_KEPT) {
        let code = name(entry, "code");
        if !token(&code) || kept.iter().any(|note| note.code == code) {
            continue;
        }
        let text = entry
            .get("text")
            .and_then(Value::as_str)
            .map(|text| cut(text, NOTE_TEXT_CHARS))
            .filter(|text| !text.is_empty());
        let values = array(entry, "parameters");
        let formatted_note = (code.starts_with("provider_stop_")
            && (3..=4).contains(&values.len()))
            || (code == "lead_member_added" && values.len() == 3);
        let parameters = if formatted_note {
            values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .filter(|value| {
                            !value.is_empty()
                                && value.len() <= 64
                                && value
                                    .bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                        })
                        .map(str::to_owned)
                })
                .collect::<Option<Vec<_>>>()
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        kept.push(ServiceNote {
            code,
            text,
            parameters,
        });
    }
    kept
}

/// A string field that may be absent: empty and missing both mean no value.
/// The most decisions a card lists: the store's bound.
pub(crate) const DECISIONS_KEPT: usize = 4;
/// The longest decision token kept.
const DECISION_CHARS: usize = 32;

/// A card's decisions as its row lists them, in order: short lowercase
/// tokens, each once, at most [`DECISIONS_KEPT`]. An entry of another shape is
/// ignored, never an error; a token this version has no words for is kept
/// here and left out by the view. A row without the list (an older hgs) takes
/// `acknowledged` alone, as the store does for a card opened without one.
pub(crate) fn decisions(row: &Value) -> Vec<String> {
    let Some(list) = row.get("decisions").and_then(Value::as_array) else {
        return vec!["acknowledged".to_owned()];
    };
    let token = |text: &str| {
        text.len() <= DECISION_CHARS
            && text.starts_with(|c: char| c.is_ascii_lowercase())
            && text
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    let mut kept: Vec<String> = Vec::new();
    for entry in list.iter().filter_map(Value::as_str) {
        if kept.len() == DECISIONS_KEPT {
            break;
        }
        if token(entry) && !kept.iter().any(|known| known == entry) {
            kept.push(entry.to_owned());
        }
    }
    kept
}

/// Who holds a queue row's attempt: absent when the row has no `holder`,
/// null when it names nobody.
pub(crate) fn holder(row: &Value) -> Option<Option<String>> {
    match row.get("holder")? {
        Value::String(member) if !member.is_empty() => Some(Some(member.clone())),
        _ => Some(None),
    }
}

pub(crate) fn optional_name(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|found| cut(found, NAME_CHARS))
        .filter(|found| !found.is_empty())
}

/// A numeric field that may be absent.
pub(crate) fn optional_count(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(|found| {
        found
            .as_u64()
            .or_else(|| found.as_f64().map(|n| n.max(0.0) as u64))
    })
}

/// A queue row's own kind: `task` or `stage`; absent means a task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    Task,
    Stage,
}

pub(crate) fn row_kind(value: &Value) -> Option<RowKind> {
    match value.get("kind").and_then(Value::as_str) {
        Some("task") => Some(RowKind::Task),
        Some("stage") => Some(RowKind::Stage),
        _ => None,
    }
}

/// A stage's kind: `review`, `test` or `land`. Any other word is kept out as
/// absent, never an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    Review,
    Test,
    Land,
}

pub(crate) fn stage_kind(value: &Value) -> Option<StageKind> {
    match value.get("kind").and_then(Value::as_str) {
        Some("review") => Some(StageKind::Review),
        Some("test") => Some(StageKind::Test),
        Some("land") => Some(StageKind::Land),
        _ => None,
    }
}

pub(crate) fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

pub(crate) fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// The cursor of the reply's next page, when it names one.
pub(crate) fn next_cursor(value: &Value) -> Option<String> {
    value
        .get("next")
        .and_then(Value::as_str)
        .filter(|next| !next.is_empty())
        .map(|next| cut(next, crate::upstream_teams::read::CURSOR_BYTES))
}

// ---------------------------------------------------------------- the list

/// One team of `hgs team list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TeamSummary {
    pub id: String,
    pub project: String,
    pub name: String,
    pub status: String,
    /// The lead's member id.
    pub lead: String,
    /// The computer the lead runs on.
    pub home: String,
    pub revision: u64,
    /// The lead's bound session: how the desktop finds a session's team.
    pub lead_session: Option<SessionRef>,
    /// The bound sessions of a planning, running or paused team's members,
    /// at most [`MEMBERS_KEPT`]; absent when the row names none, as an older
    /// command line's row does, so the serialized summary is unchanged then.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_sessions: Option<Vec<MemberSession>>,
    /// Bounded list projection; absent from an older CLI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<TeamProgress>,
}

/// The list's capped counts. A `*_more` flag denotes a lower bound.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TeamProgress {
    pub tasks: u64,
    pub accepted: u64,
    pub questions: u64,
    pub members: u64,
    pub phases: u64,
    pub phases_done: u64,
    pub stuck: bool,
    pub tasks_more: bool,
    pub accepted_more: bool,
    pub questions_more: bool,
    pub members_more: bool,
    pub phases_more: bool,
    pub phases_done_more: bool,
}

/// One member's bound session on a live team's list row: how Waiting for you
/// knows a member's session without loading the team.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MemberSession {
    pub member: String,
    /// Empty for an older command line that did not include the role.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub role: String,
    pub machine: String,
    pub session_id: String,
}

impl TeamSummary {
    pub fn draft(&self) -> bool {
        self.status == "draft"
    }
}

/// The teams of this computer, as the store lists them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TeamList {
    pub teams: Vec<TeamSummary>,
    pub truncated: bool,
    /// The cursor of the next page, when the reply names one.
    pub next: Option<String>,
    /// The cursor this page was read from; absent for the first page, so a
    /// view can keep pages by their cursor and tell a later page apart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

// --------------------------------------------------------------- one team

/// A member's session: the computer UUID and the session identity
/// (`launch_id:<uuid>` or `run_id:<id>`). A name is never an identity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SessionRef {
    pub machine: String,
    pub session_id: String,
}

/// The lead's bound session of a list row; absent when the row has none.
pub(crate) fn lead_session(row: &Value) -> Option<SessionRef> {
    let found = row.get("lead_session").filter(|lead| lead.is_object())?;
    let view = SessionRef {
        machine: name(found, "machine"),
        session_id: name(found, "session_id"),
    };
    (!view.session_id.is_empty()).then_some(view)
}

/// A list row's `member_sessions`: entries that name a member, a machine and a
/// session, at most [`MEMBERS_KEPT`]; none when the row has no such array.
pub(crate) fn member_sessions(row: &Value) -> Option<Vec<MemberSession>> {
    let found = row.get("member_sessions")?.as_array()?;
    Some(
        found
            .iter()
            .filter_map(|entry| {
                let session = MemberSession {
                    member: name(entry, "member"),
                    role: name(entry, "role"),
                    machine: name(entry, "machine"),
                    session_id: name(entry, "session_id"),
                };
                (!session.member.is_empty()
                    && !session.machine.is_empty()
                    && !session.session_id.is_empty())
                .then_some(session)
            })
            .take(MEMBERS_KEPT)
            .collect(),
    )
}

pub(crate) fn session(value: &Value) -> Option<SessionRef> {
    let found = value.get("session").filter(|session| session.is_object())?;
    let view = SessionRef {
        machine: name(found, "machine"),
        session_id: name(found, "session_id"),
    };
    (!view.session_id.is_empty()).then_some(view)
}

/// One member of a team, as the Team tab's members column shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MemberCard {
    pub id: String,
    pub role: String,
    pub agent: String,
    pub account: String,
    /// `not_created`, `active`, `paused`, `removed`.
    pub state: String,
    pub lead: bool,
    pub session: Option<SessionRef>,
    /// The model its role names, when it names one.
    pub model: Option<String>,
    /// The person stopped it: the store refuses a second Stop (absent on an
    /// older hgs's row: false).
    pub person_stopped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_choice: Option<zerus_team_plan::catalog::AccountChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_work: Option<u64>,
}

/// The member counts of `team status`'s `counts.members`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MemberCounts {
    pub total: u64,
    pub not_created: u64,
    pub active: u64,
    pub paused: u64,
    pub removed: u64,
}

/// The node counts of `team status`'s `counts.nodes`, by state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct NodeCounts {
    pub waiting: u64,
    pub pending: u64,
    pub in_progress: u64,
    pub reported: u64,
    pub accepted: u64,
    pub cancelled: u64,
}

/// The numbers the team bar shows: `team status`'s `counts`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TeamCounts {
    pub members: MemberCounts,
    pub nodes: NodeCounts,
    /// The lead's open inbox items.
    pub inbox: u64,
    /// The person's open cards.
    pub cards: u64,
}

/// One stage of a card: the review or test node its pipeline created.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StageCard {
    pub node: u64,
    /// The position in the pipeline.
    pub index: u64,
    pub role: String,
    /// The node state (`waiting`, `pending`, `in_progress`, `reported`,
    /// `accepted`, `cancelled`).
    pub state: String,
    /// The member the stage is assigned to: who does the stage.
    pub assignee: String,
    /// A verified delivered holder already present in the CLI row. Omitted
    /// for older replies, no holder, or an invalid/unbounded member name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder: Option<String>,
    /// The member who implemented the task the stage checks (the plan's
    /// binding decision). Never the stage's own worker.
    pub implementer: String,
    /// `review`, `test` or `land`; absent when the row has no `stage` object or
    /// names another kind.
    pub kind: Option<StageKind>,
    /// The node this stage checks.
    pub target: Option<u64>,
    /// The target's round this stage checks.
    pub round: Option<u64>,
}

/// The task a card stands for: the replicated task, never the node alone.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TaskRefView {
    pub project: String,
    pub uuid: String,
}

pub(crate) fn task_ref(node: &Value) -> Option<TaskRefView> {
    let task = node.get("task").filter(|task| task.is_object())?;
    let view = TaskRefView {
        project: name(task, "project"),
        uuid: name(task, "uuid"),
    };
    (!view.uuid.is_empty()).then_some(view)
}

/// One board card: a node of the plan or the queue and, when a pipeline runs,
/// its stages. A card is never a stage node of another card.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct QueueCard {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_base: Option<String>,
    /// Store-owned pre-start predicate; an older read grants no edit affordance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deps_editable: Option<bool>,
    pub node: u64,
    /// Dependency facts are kept even when the other endpoint is off-page.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<zerus_team_plan::model::Dep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<u32>,
    /// The longest dependency path within this bounded queue page; absent
    /// at layer zero, preserving older empty-edge view shapes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<u32>,
    pub title: String,
    pub state: String,
    pub task: Option<TaskRefView>,
    pub role: String,
    pub pipeline: String,
    pub assignee: String,
    pub implementer: String,
    /// For a stage card, the members who implemented any round of its
    /// target, whom the stage's Reassign refuses (zerus-3u5yu). Empty, and
    /// omitted, for any other card and for older replies.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub earlier_implementers: Vec<String>,
    /// The task's board rank: the queue's order.
    pub rank: String,
    pub round: u64,
    /// The queue row's own kind; absent means a task.
    pub kind: Option<RowKind>,
    /// The pinned pipeline's rounds, when the row names them.
    pub rounds: Option<u64>,
    pub held: bool,
    pub admitted: bool,
    pub integrated: bool,
    pub decided_by: String,
    pub stages: Vec<StageCard>,
    /// Who holds the task's attempt, the holder the store's decisions use:
    /// absent when the row does not say (an older hgs), null when nobody does
    /// (the store then refuses a Return).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder: Option<Option<String>>,
    /// The kind of the task's decision that has not settled, when there is
    /// one: the store refuses accept, return, release and cancel meanwhile,
    /// unless the pending decision can still be replaced.
    pub pending_decision: Option<String>,
    /// The pending decision has not started and a new Return, Reassign or
    /// Cancel replaces it (zerus-63rru.43, computed by the store with the
    /// operation's own predicate); false when nothing is pending, and when an
    /// older hgs does not say.
    pub pending_replaceable: bool,
    /// What a stopped or archived team's stop did to this node, as a
    /// stable code plus the store's refusal reason as data (zerus-63rru.52
    /// round 2); absent for a live team's rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_outcome: Option<StopOutcomeCard>,
}
/// A stop outcome's code and reason (zerus-63rru.52 round 2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StopOutcomeCard {
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One group of the board: its state and its cards.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct BoardGroup {
    pub state: String,
    pub cards: Vec<QueueCard>,
}

/// The team's bar: what the tab shows above the board.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TeamBar {
    pub id: String,
    pub project: String,
    pub name: String,
    pub status: String,
    pub goal: String,
    pub lead: String,
    pub integration_branch: String,
    pub base_commit: String,
    pub revision: u64,
    pub counts: TeamCounts,
}

/// A role and pipeline from this team's pinned bundle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RoutingPipeline {
    pub id: String,
    pub name: String,
    pub role: String,
}

/// `hgs team status` and `hgs team queue` for the Team tab.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TeamView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planning: Option<planning::PlanningState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<phases::PhasePane>,
    pub bar: TeamBar,
    /// Complete settings for editing; absent if this version cannot read every field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<zerus_team_plan::Settings>,
    pub members: Vec<MemberCard>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub member_roles: Vec<MemberRole>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub routing_pipelines: Vec<RoutingPipeline>,
    /// Complete bounded goal for editing; the bar keeps its display preview.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edit_goal: Option<String>,
    pub groups: Vec<BoardGroup>,
    /// Cards or stages over a bound were left out.
    pub truncated: bool,
    /// The cursor of the queue's next page, when the reply names one.
    pub next: Option<String>,
    pub revision: u64,
    /// The queue page's cursor; absent for the first page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

// ----------------------------------------------------------------- waiting

/// One thing that waits for the person: an open owner card.
///
/// A request that only the person can answer inside a team *session* is not
/// here: the desktop's session attention data already carries it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct WaitingItem {
    /// Team and card sequence, for the decision that answers it.
    pub id: String,
    pub team: String,
    pub card: u64,
    pub kind: String,
    pub node: Option<u64>,
    pub text: String,
    pub state: String,
    /// The person the card is for, from the record's actor.
    pub by: String,
    pub created_ms: i64,
    /// The decisions the card takes, in the row's order (see [`decisions`]).
    pub decisions: Vec<String>,
    /// The service's statements about a report the card quotes, apart from
    /// the member's text (see [`ServiceNote`]). Empty for every other card,
    /// and left out then so an unchanged card serializes as before.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<ServiceNote>,
    /// The card's bounded parameters the service's writer sent
    /// (zerus-63rru.34), in the three states the window tells apart: absent
    /// (a missing or null field), [`CardParameters::Pairs`] (the writer's
    /// bounded list) and [`CardParameters::Invalid`] (anything else). An
    /// older store has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<CardParameters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_reference: Option<PhaseReference>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseReference {
    pub phase: u32,
    pub generation: u64,
    pub purpose: String,
}

/// A card row's parameters, kept in one of the three states the window must
/// tell apart (zerus-63rru.34, F6). The core never drops, cuts or normalizes
/// invalid metadata: an invalid row makes the window show the card's stored
/// sentence instead of substituting a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardParameters {
    /// The service writer's bounded pair list, possibly empty.
    Pairs(Vec<(String, String)>),
    /// The row's metadata is not that pair list.
    Invalid,
}

impl Serialize for CardParameters {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Pairs(pairs) => pairs.serialize(serializer),
            // A marker the window cannot mistake for a pair list. The card's
            // stored sentence stands for it.
            Self::Invalid => serializer.serialize_str("invalid"),
        }
    }
}

/// `hgs team waiting`: the open owner cards of every team, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct WaitingView {
    pub items: Vec<WaitingItem>,
    /// The cursor of the next page, when the reply names one.
    pub next: Option<String>,
    /// The cursor this page was read from; absent for the first page, which
    /// alone can say that nothing waits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

#[cfg(test)]
mod tests;

// The fork keeps upstream's team client unwired (merge 2026-11-17): these
// tests drive it through the client-wide Change pipeline or the removed UI
// fixtures, which carry the fork's own teams.
#[cfg(any())]
mod progress;
// The fork keeps upstream's team client unwired (merge 2026-11-17): these
// tests drive it through the client-wide Change pipeline or the removed UI
// fixtures, which carry the fork's own teams.
#[cfg(any())]
mod session_groups;
