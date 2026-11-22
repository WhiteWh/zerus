use super::DecisionState;
use crate::teams::ops::ReportNote;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zerus_team_plan::model::TeamStatus;

pub const MAX_PAGE: u32 = 200;
pub const LIVE_JOURNAL_CAP: u64 = 10_000;

/// The bound part of one roster, without accounts, paths or member state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberSession {
    pub member: String,
    #[serde(default)]
    pub role: String,
    pub machine: String,
    pub session_id: String,
}

/// A launch-bound member's durable handle (zerus-63rru.53): the session ID
/// of the record its binding verified, the launch it was bound to and the
/// session's name at binding, a route for when the ID index misses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberHandle {
    pub machine: String,
    pub session_id: String,
    pub launch: String,
    pub label_hint: String,
}

/// Snapshot high-water mark excludes additions while the person turns pages.
/// The scope binds the cursor to the team, list and filter that issued it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub scope: String,
    pub ceiling: u64,
    pub after: u64,
    #[serde(default)]
    pub rank: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page<T> {
    pub rows: Vec<T>,
    pub next: Option<Cursor>,
}
/// A queue row with what a view needs to foresee a refusal: the member
/// holding the attempt (none when no one does), the kind of a pending
/// decision (none when nothing is pending), and whether a new Return,
/// Reassign or Cancel would replace that decision instead of being refused
/// (`DecisionState::replaceable`; false when nothing is pending), and the
/// sentence of the lead's open item when no member can take the ready node
/// (none otherwise).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueRow {
    pub assigned_base: Option<String>,
    /// Store-owned pre-start/attempt/effect predicate, from this page's snapshot.
    pub deps_editable: bool,
    pub node: zerus_team_plan::model::Node,
    pub holder: Option<String>,
    pub pending: Option<String>,
    pub pending_replaceable: bool,
    pub unstaffed: Option<String>,
    /// For a stage, the members who implemented any round of its target, whom
    /// its Reassign refuses (zerus-3u5yu); empty for any other row.
    pub earlier_implementers: Vec<String>,
    /// The stop facts the store keeps for the node (zerus-63rru.52): the
    /// newest recorded cancel, the key clear, and whether a cancel is still
    /// pending. None until the page's query fills them.
    pub stop: Option<StopFacts>,
}
/// What the stop's records say about a node, read from the store's own
/// tables, never inferred from the node's state (zerus-63rru.52).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct StopFacts {
    pub cancel: Option<String>,
    pub cancel_reason: Option<String>,
    pub key: Option<String>,
    pub key_reason: Option<String>,
    pub pending_cancel: bool,
    /// The team's recorded stop sequence; zero when no Stop of the new kind
    /// has run (zerus-63rru.52 round 4): such a team shows no outcome.
    pub stop_seq: i64,
}
/// A member row with whether the person stopped the member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberRow {
    pub member: zerus_team_plan::model::Member,
    pub person_stopped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxItem {
    pub seq: u64,
    pub kind: String,
    pub node: u32,
    pub generation: u32,
    pub actor: Value,
    pub text: String,
    /// Service statements beside an inbox item. Rows from before notes have none.
    #[serde(default)]
    pub notes: Vec<ReportNote>,
    pub state: String,
    pub reply: Option<String>,
    pub reply_actor: Option<Value>,
    pub created_ms: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Card {
    pub seq: u64,
    pub kind: String,
    pub node: Option<u32>,
    pub actor: Value,
    pub text: String,
    pub state: String,
    pub decision: Option<String>,
    pub decided_actor: Option<Value>,
    pub created_ms: i64,
    /// The decisions the card takes: those its opener listed, or for a card
    /// opened without a list (and every row from before lists existed) the
    /// rule of its kind.
    #[serde(default)]
    pub decisions: Vec<String>,
    /// The service's statements about a report the card quotes, apart from
    /// the member's text. Every other card has none.
    #[serde(default)]
    pub notes: Vec<ReportNote>,
    /// The bounded parameters a localized window substitutes into the card's
    /// words: at most 8 keys of [a-z_], each value at most 256 characters
    /// after the sentence's forbidden-character rules, written only by the
    /// service's own card writer (zerus-63rru.34). A row from before the
    /// field reads as none.
    #[serde(default)]
    pub parameters: Vec<(String, String)>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalEntry {
    pub seq: u64,
    pub at_ms: i64,
    pub actor: Value,
    pub kind: String,
    pub request_id: String,
    pub text: String,
    pub refs: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectIntent {
    pub seq: u64,
    pub node: u32,
    pub member: String,
    pub generation: u32,
    pub request_id: String,
    pub actor: Value,
    pub state: String,
    pub kind: String,
    pub outcome: Option<Value>,
    pub created_ms: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub state: DecisionState,
    pub seq: u64,
    pub node: u32,
    pub generation: u32,
    pub actor: Value,
    pub kind: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingSuggestion {
    pub role: Option<String>,
    pub pipeline: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationFact {
    pub decision: u64,
    pub node: u32,
    pub actor: Value,
    pub commit: Option<String>,
    pub conflicts: Vec<String>,
    pub created_ms: i64,
}

/// An exact acceptance identity for a service integration attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationAccept {
    pub seq: u64,
    pub node: u32,
    pub generation: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptHolder {
    pub member: String,
    pub generation: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamRow {
    pub id: String,
    pub project: String,
    pub name: String,
    pub status: TeamStatus,
    pub lead: String,
    pub home: String,
    pub revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitingCard {
    pub team: String,
    pub card: Card,
}

/// A task creation that the service must perform once or reconcile as uncertain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageTask {
    pub node: u32,
    pub target: u32,
    /// Generation of the target report that asked for this stage.
    pub generation: u32,
    pub state: String,
    pub request_id: Option<String>,
}

/// One snapshot supplies the native stage writer's exact reserved authority
/// and content. The writer also verifies that home is this computer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageTaskEffect {
    pub team: String,
    pub node: u32,
    pub request_id: String,
    pub home: String,
    pub project: String,
    pub title: String,
    pub text: String,
    pub kind: zerus_team_plan::model::NodeKind,
    pub role: String,
    pub target: zerus_team_plan::model::TaskRef,
    pub target_title: String,
    pub round: u32,
    /// Generation of the target, not of the stage's own later hand-out.
    pub generation: u32,
}
