//! What may run now, and which member takes it.
//!
//! A node is ready when all of these hold:
//!
//! - it is Pending (its task is in To do) and not held;
//! - its phase exists, is not paused and is Approved, and a queue node is
//!   admitted;
//! - no planned phase before its own awaits acceptance;
//! - in a barrier phase, no planned phase before it has an undecided node;
//! - every hard dependency is Accepted and, in a team with an integration
//!   branch, integrated; every soft one is at least In progress; a cancelled
//!   one releases; a dependency missing from the view, listed twice in it,
//!   or naming a stage, holds;
//! - it is in no dependency cycle among the nodes that are not cancelled;
//! - no node In progress or Reported has a write set that overlaps its own;
//! - a stage checks a node that is Reported, in the stage's round, and
//!   listed once;
//! - its ID appears once in the view: a node listed twice is never handed
//!   out, since either row may be the stale one.
//!
//! "Before" follows the phases' display order: `order`, or the phase number
//! when `order` is unset. The queue is no planned phase: it neither waits
//! for one nor holds one.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::graph;
use crate::model::{
    DepKind, Integration, MemberId, MemberState, Node, NodeId, NodeKind, NodeState, Phase,
    PhaseKind, PhaseState,
};
use crate::overlap::overlaps;

/// A member as hand-out sees it. `free` is decided by the caller from the
/// member's session and task index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberView {
    pub id: MemberId,
    pub role: String,
    pub state: MemberState,
    pub free: bool,
}

/// The rows a decision reads: the team's phases, its live nodes with every
/// node they depend on, and its members.
#[derive(Clone, Copy, Debug)]
pub struct PlanView<'a> {
    pub phases: &'a [Phase],
    pub nodes: &'a [Node],
    pub members: &'a [MemberView],
    /// The lead's member ID. The lead never takes a task.
    pub lead: &'a str,
    /// The team's integration mode: whether accepted work is merged into an
    /// integration branch before a hard dependency on it releases.
    pub integration: Integration,
}

/// One task for one member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handout {
    pub node: NodeId,
    pub member: MemberId,
}

/// A ready node without staffing, or whose eligible members are all unreadable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unstaffed {
    pub node: NodeId,
    pub reason: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unreadable: bool,
}

/// The nodes that may start now, in hand-out order: stages first, oldest
/// first, because they unblock work already done; then planned nodes, the
/// one with the most transitive dependants first and then by ID; then queue
/// nodes in board order, by rank and then task UUID.
pub fn ready(view: &PlanView) -> Vec<NodeId> {
    let index = Index::new(view);
    index.ready().iter().map(|node| node.id).collect()
}

/// The hand-outs to make now, in [`ready`] order. A member qualifies when it
/// is Active and free, holds no node In progress, is not the lead, has the
/// node's role, and is the node's assignee when one is named. A stage never
/// goes to the member who implemented its node. Each member gets at most one
/// node, two nodes whose write sets overlap are never handed out together,
/// and a node no member qualifies for is skipped.
pub fn handouts(view: &PlanView) -> Vec<Handout> {
    let index = Index::new(view);
    let busy = busy(view);
    let mut given: BTreeSet<&str> = BTreeSet::new();
    let mut claimed: Vec<&[String]> = Vec::new();
    let mut out = Vec::new();
    for node in index.ready() {
        if claimed.iter().any(|writes| overlaps(&node.writes, writes)) {
            continue;
        }
        let work = index.work(node);
        let taker = view.members.iter().find(|member| {
            member.state == MemberState::Active
                && member.free
                && !busy.contains(member.id.as_str())
                && !given.contains(member.id.as_str())
                && index.staffing.may_take(member, &work)
        });
        if let Some(member) = taker {
            given.insert(&member.id);
            claimed.push(&node.writes);
            out.push(Handout {
                node: node.id,
                member: member.id.clone(),
            });
        }
    }
    out
}

/// Ready nodes that no member could take, or for which every eligible member
/// is marked unreadable. A busy, paused or not yet created member still
/// counts as one who could take the task.
pub fn unstaffed(view: &PlanView, marked: &BTreeSet<MemberId>) -> Vec<Unstaffed> {
    let index = Index::new(view);
    index
        .ready()
        .into_iter()
        .filter_map(|node| {
            let work = index.work(node);
            if let Some(reason) = index.staffing.gap(&work) {
                return Some(Unstaffed {
                    node: node.id,
                    reason,
                    unreadable: false,
                });
            }
            if marked.is_empty() {
                return None;
            }
            let eligible: Option<BTreeSet<_>> = view
                .members
                .iter()
                .filter(|member| {
                    member.state != MemberState::Removed && index.staffing.may_take(member, &work)
                })
                .map(|member| {
                    (member.state == MemberState::Active && marked.contains(&member.id))
                        .then_some(member.id.as_str())
                })
                .collect();
            let members = eligible?.into_iter().collect::<Vec<_>>().join(", ");
            Some(Unstaffed {
                node: node.id,
                reason: format!("The service cannot read any eligible member ({members})."),
                unreadable: true,
            })
        })
        .collect()
}

/// A piece of work as staffing sees it.
pub(crate) struct Work<'a> {
    pub role: &'a str,
    pub assignee: Option<&'a str>,
    /// Members who implemented what a stage checks; they never take it.
    pub implementers: Vec<&'a str>,
}

/// Which members may ever take a piece of work, whatever their load.
pub(crate) struct Staffing<'a> {
    pub view: PlanView<'a>,
}

impl Staffing<'_> {
    /// Whether `member` may take `work`, whatever its state and load.
    pub(crate) fn may_take(&self, member: &MemberView, work: &Work) -> bool {
        member.id != self.view.lead
            && !member.role.is_empty()
            && member.role == work.role
            && work.assignee.is_none_or(|assignee| assignee == member.id)
            && !work.implementers.contains(&member.id.as_str())
    }

    /// Why no member who is not removed could ever take `work`, or `None`
    /// when one could. A busy, paused or not yet created member could.
    pub(crate) fn gap(&self, work: &Work) -> Option<String> {
        let members = self.view.members.iter();
        let mut able = members.filter(|member| member.state != MemberState::Removed);
        if able.any(|member| self.may_take(member, work)) {
            return None;
        }
        let role = work.role;
        if role.is_empty() {
            return Some("The task names no role.".to_owned());
        }
        let of_role: Vec<&str> = self
            .view
            .members
            .iter()
            .filter(|member| {
                member.state != MemberState::Removed
                    && member.id != self.view.lead
                    && member.role == role
            })
            .map(|member| member.id.as_str())
            .collect();
        if of_role.is_empty() {
            return Some(format!("No member of the team has the role {role}."));
        }
        const RULE: &str =
            "a stage is never handed to a member who implemented any round of the task";
        Some(match work.assignee {
            Some(assignee) if !of_role.contains(&assignee) => {
                format!("The assignee {assignee} is not a member with the role {role}.")
            }
            Some(assignee) => format!(
                "The assignee {assignee} implemented the task, and {RULE}. The task went to the lead, who can accept, return or cancel it there."
            ),
            None if of_role.len() == 1 => format!(
                "{} implemented the task and is the only member with the role {role}, and {RULE}. The task went to the lead, who can accept, return or cancel it there.",
                of_role[0]
            ),
            None => format!(
                "{} implemented every round of the task and are the only members with the role {role}, and {RULE}. The task went to the lead, who can accept, return or cancel it there.",
                of_role.join(" and ")
            ),
        })
    }
}

/// Members holding a node In progress: its assignee, and the implementer of
/// a node that is not a stage. Hand-out does not trust `free` alone for them.
fn busy<'a>(view: &PlanView<'a>) -> BTreeSet<&'a str> {
    let mut busy = BTreeSet::new();
    for node in view
        .nodes
        .iter()
        .filter(|node| node.state == NodeState::InProgress)
    {
        busy.extend(node.assignee.as_deref());
        if node.kind != NodeKind::Stage {
            busy.extend(node.implementer.as_deref());
        }
    }
    busy
}

/// A phase's place in the display order.
fn position(phase: &Phase) -> (u32, u32) {
    let order = if phase.order == 0 {
        phase.n
    } else {
        phase.order
    };
    (order, phase.n)
}

/// The lookups one decision needs, built once per call.
struct Index<'a> {
    view: PlanView<'a>,
    staffing: Staffing<'a>,
    nodes: BTreeMap<NodeId, &'a Node>,
    /// IDs that more than one row of the view carries.
    duplicates: BTreeSet<NodeId>,
    phases: BTreeMap<u32, &'a Phase>,
    cycles: BTreeSet<NodeId>,
    /// The first planned phase that awaits acceptance.
    awaiting: Option<(u32, u32)>,
    /// The first place of an undecided node in a planned phase that is not
    /// rejected; a node of a missing phase counts by its phase number.
    undecided: Option<(u32, u32)>,
    /// Write sets of nodes In progress or Reported.
    open_writes: Vec<(NodeId, &'a [String])>,
}

impl<'a> Index<'a> {
    fn new(view: &PlanView<'a>) -> Self {
        let mut nodes: BTreeMap<NodeId, &Node> = BTreeMap::new();
        let mut duplicates = BTreeSet::new();
        for node in view.nodes {
            if nodes.insert(node.id, node).is_some() {
                duplicates.insert(node.id);
            }
        }
        let phases: BTreeMap<u32, &Phase> =
            view.phases.iter().map(|phase| (phase.n, phase)).collect();
        let awaiting = phases
            .values()
            .filter(|phase| {
                phase.kind == PhaseKind::Plan && phase.state == PhaseState::AwaitingAcceptance
            })
            .map(|phase| position(phase))
            .min();
        let undecided = view
            .nodes
            .iter()
            .filter(|node| !matches!(node.state, NodeState::Accepted | NodeState::Cancelled))
            .filter_map(|node| match phases.get(&node.phase) {
                Some(phase)
                    if phase.kind == PhaseKind::Queue || phase.state == PhaseState::Rejected =>
                {
                    None
                }
                Some(phase) => Some(position(phase)),
                None => Some((node.phase, node.phase)),
            })
            .min();
        let cycles = graph::cycle_members(
            view.nodes
                .iter()
                .filter(|node| node.state != NodeState::Cancelled),
        );
        let open_writes = view
            .nodes
            .iter()
            .filter(|node| matches!(node.state, NodeState::InProgress | NodeState::Reported))
            .filter(|node| !node.writes.is_empty())
            .map(|node| (node.id, node.writes.as_slice()))
            .collect();
        Self {
            view: *view,
            staffing: Staffing { view: *view },
            nodes,
            duplicates,
            phases,
            cycles,
            awaiting,
            undecided,
            open_writes,
        }
    }

    /// The ready nodes in hand-out order.
    fn ready(&self) -> Vec<&'a Node> {
        let mut stages = Vec::new();
        let mut planned = Vec::new();
        let mut queued = Vec::new();
        for node in self.view.nodes.iter().filter(|node| self.is_ready(node)) {
            if node.kind == NodeKind::Stage {
                stages.push(node);
            } else if self.phases[&node.phase].kind == PhaseKind::Queue {
                queued.push(node);
            } else {
                planned.push(node.id);
            }
        }
        stages.sort_by_key(|node| node.id);
        if planned.len() > 1 {
            planned = graph::dispatch_order(self.view.nodes, &planned);
        }
        let board = |node: &&'a Node| {
            let uuid = node.task.as_ref().map_or("", |task| task.uuid.as_str());
            (node.rank.as_str(), uuid, node.id)
        };
        queued.sort_by(|a, b| board(a).cmp(&board(b)));
        stages
            .into_iter()
            .chain(planned.iter().map(|id| self.nodes[id]))
            .chain(queued)
            .collect()
    }

    fn is_ready(&self, node: &Node) -> bool {
        node.state == NodeState::Pending
            && !node.hold
            && self.phase_open(node)
            && !self.after_awaited_phase(node)
            && !self.behind_barrier(node)
            && node
                .deps
                .iter()
                .all(|dep| self.dependency_met(dep.node, dep.kind))
            && !self.cycles.contains(&node.id)
            && !self.writes_held(node)
            && self.stage_target_reported(node)
            && !self.duplicates.contains(&node.id)
    }

    fn phase_open(&self, node: &Node) -> bool {
        self.phases.get(&node.phase).is_some_and(|phase| {
            !phase.paused
                && phase.state == PhaseState::Approved
                && (phase.kind != PhaseKind::Queue || node.admitted)
        })
    }

    fn after_awaited_phase(&self, node: &Node) -> bool {
        self.planned_place(node)
            .is_some_and(|place| self.awaiting.is_some_and(|awaiting| awaiting < place))
    }

    fn behind_barrier(&self, node: &Node) -> bool {
        let barrier = self
            .phases
            .get(&node.phase)
            .is_some_and(|phase| phase.barrier);
        barrier
            && self
                .planned_place(node)
                .is_some_and(|place| self.undecided.is_some_and(|undecided| undecided < place))
    }

    /// The place of the node's phase when it is a planned phase.
    fn planned_place(&self, node: &Node) -> Option<(u32, u32)> {
        self.phases
            .get(&node.phase)
            .filter(|phase| phase.kind == PhaseKind::Plan)
            .map(|phase| position(phase))
    }

    fn dependency_met(&self, id: NodeId, kind: DepKind) -> bool {
        let Some(dep) = self.nodes.get(&id) else {
            return false;
        };
        // Which of two rows is true is unknown, so neither releases.
        if self.duplicates.contains(&id) {
            return false;
        }
        // A stage is never a dependency; naming one holds, like a missing node.
        if dep.kind == NodeKind::Stage {
            return false;
        }
        match (dep.state, kind) {
            (NodeState::Cancelled, _) => true,
            (NodeState::Accepted, DepKind::Hard) => {
                self.view.integration == Integration::None || dep.integrated
            }
            (_, DepKind::Hard) => false,
            (state, DepKind::Soft) => matches!(
                state,
                NodeState::InProgress | NodeState::Reported | NodeState::Accepted
            ),
        }
    }

    fn writes_held(&self, node: &Node) -> bool {
        !node.writes.is_empty()
            && self
                .open_writes
                .iter()
                .any(|(id, writes)| *id != node.id && overlaps(&node.writes, writes))
    }

    /// A stage checks a report that is current: its node is Reported in the
    /// stage's round and listed once. A stage of an earlier round is obsolete.
    fn stage_target_reported(&self, node: &Node) -> bool {
        if node.kind != NodeKind::Stage {
            return true;
        }
        let Some(id) = node.stage_of else {
            return false;
        };
        if self.duplicates.contains(&id) {
            return false;
        }
        self.nodes.get(&id).is_some_and(|target| {
            target.state == NodeState::Reported && target.round.max(1) == node.round.max(1)
        })
    }

    /// The node as staffing sees it. A stage never goes to the member who
    /// implemented the node it checks, by its own record or the node's.
    fn work(&self, node: &'a Node) -> Work<'a> {
        let mut implementers = Vec::new();
        if node.kind == NodeKind::Stage {
            implementers.extend(node.implementer.as_deref());
            let target = node.stage_of.and_then(|id| self.nodes.get(&id));
            implementers.extend(target.and_then(|target| target.implementer.as_deref()));
            if let Some(target) = target {
                implementers.extend(target.earlier_implementers.iter().map(|m| m.as_str()));
            }
        }
        Work {
            role: &node.role,
            assignee: node.assignee.as_deref(),
            implementers,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::model::{NodeState, PhaseState, TeamFixture, LEAD};
    use crate::testkit::{member, node, phase, queued, Plan};

    fn is_ready(plan: &Plan, id: NodeId) -> bool {
        ready(&plan.view()).contains(&id)
    }

    fn given(plan: &Plan) -> Vec<(NodeId, String)> {
        handouts(&plan.view())
            .into_iter()
            .map(|handout| (handout.node, handout.member))
            .collect()
    }

    fn pairs(list: &[(NodeId, &str)]) -> Vec<(NodeId, String)> {
        list.iter()
            .map(|(node, member)| (*node, member.to_string()))
            .collect()
    }

    fn reported(id: NodeId, implementer: &str) -> Node {
        node(id, json!({"state": "reported", "implementer": implementer}))
    }

    fn stage(id: NodeId, of: NodeId, implementer: &str) -> Node {
        node(
            id,
            json!({"kind": "stage", "stage_of": of, "role": "reviewer", "implementer": implementer}),
        )
    }

    // Readiness: each test starts from a ready node and breaks one condition.

    #[test]
    fn only_a_task_in_to_do_is_ready() {
        let mut plan = Plan::new(vec![node(1, json!({}))]);
        assert!(is_ready(&plan, 1));
        for state in [
            NodeState::Draft,
            NodeState::Waiting,
            NodeState::InProgress,
            NodeState::Reported,
            NodeState::Accepted,
            NodeState::Cancelled,
        ] {
            plan.node(1).state = state;
            assert!(!is_ready(&plan, 1), "{state:?}");
        }
    }

    #[test]
    fn a_held_task_is_not_ready() {
        let mut plan = Plan::new(vec![node(1, json!({}))]);
        assert!(is_ready(&plan, 1));
        plan.node(1).hold = true;
        assert!(!is_ready(&plan, 1));
    }

    #[test]
    fn a_planned_task_waits_for_its_phase_to_be_approved() {
        let mut plan = Plan::new(vec![node(1, json!({}))]);
        assert!(is_ready(&plan, 1));
        for state in [
            PhaseState::Draft,
            PhaseState::AwaitingAcceptance,
            PhaseState::Done,
            PhaseState::Rejected,
        ] {
            plan.phase(1).state = state;
            assert!(!is_ready(&plan, 1), "{state:?}");
        }
    }

    #[test]
    fn a_paused_phase_hands_nothing_out() {
        let mut plan = Plan::new(vec![node(1, json!({})), queued(2, "a1")]);
        assert!(is_ready(&plan, 1) && is_ready(&plan, 2));
        plan.phase(1).paused = true;
        assert!(!is_ready(&plan, 1));
        assert!(is_ready(&plan, 2), "the queue is another phase");
        plan.phase(0).paused = true;
        assert!(!is_ready(&plan, 2));
    }

    #[test]
    fn a_queue_task_waits_until_it_is_admitted() {
        let mut plan = Plan::new(vec![queued(1, "a1")]);
        assert!(is_ready(&plan, 1));
        plan.node(1).admitted = false;
        assert!(!is_ready(&plan, 1));
    }

    #[test]
    fn the_queue_hands_out_only_while_it_is_approved() {
        let mut plan = Plan::new(vec![queued(1, "a1")]);
        assert!(is_ready(&plan, 1));
        for state in [PhaseState::Draft, PhaseState::Done, PhaseState::Rejected] {
            plan.phase(0).state = state;
            assert!(!is_ready(&plan, 1), "{state:?}");
        }
    }

    #[test]
    fn a_task_of_an_unknown_phase_is_not_ready() {
        let mut plan = Plan::new(vec![node(1, json!({}))]);
        assert!(is_ready(&plan, 1));
        plan.node(1).phase = 7;
        assert!(!is_ready(&plan, 1));
    }

    #[test]
    fn nothing_starts_after_a_phase_that_awaits_acceptance() {
        let mut plan = Plan::new(vec![
            node(1, json!({"state": "accepted", "integrated": true})),
            node(2, json!({"phase": 2})),
            node(3, json!({"phase": 3})),
            queued(4, "a1"),
        ]);
        plan.phases.push(phase(2, json!({})));
        plan.phases.push(phase(3, json!({})));
        assert!(is_ready(&plan, 2) && is_ready(&plan, 3));
        plan.phase(1).state = PhaseState::AwaitingAcceptance;
        assert!(!is_ready(&plan, 2) && !is_ready(&plan, 3));
        assert!(is_ready(&plan, 4), "the queue is not after a planned phase");
        // Order follows the phases' display order, not their numbers.
        plan.phase(1).state = PhaseState::Done;
        plan.phase(2).order = 5;
        plan.phase(2).state = PhaseState::AwaitingAcceptance;
        plan.node(2).state = NodeState::Accepted;
        assert!(is_ready(&plan, 3), "phase 3 comes before phase 2 now");
    }

    #[test]
    fn a_barrier_phase_waits_for_every_earlier_task() {
        let mut plan = Plan::new(vec![node(1, json!({})), node(2, json!({"phase": 2}))]);
        plan.phases.push(phase(2, json!({})));
        assert!(is_ready(&plan, 2), "no barrier: phases run alongside");
        plan.phase(2).barrier = true;
        assert!(!is_ready(&plan, 2));
        assert!(is_ready(&plan, 1));
        for state in [NodeState::Draft, NodeState::InProgress, NodeState::Reported] {
            plan.node(1).state = state;
            assert!(!is_ready(&plan, 2), "{state:?}");
        }
        plan.node(1).state = NodeState::Accepted;
        assert!(is_ready(&plan, 2));
        plan.node(1).state = NodeState::Cancelled;
        assert!(is_ready(&plan, 2));
    }

    #[test]
    fn a_barrier_ignores_its_own_phase_the_queue_and_a_rejected_draft() {
        let mut plan = Plan::new(vec![
            node(1, json!({"state": "draft"})),
            node(2, json!({"phase": 2})),
            node(3, json!({"phase": 2})),
            queued(4, "a1"),
        ]);
        plan.phases.push(phase(2, json!({"barrier": true})));
        assert!(!is_ready(&plan, 2), "a draft of an earlier phase holds");
        plan.phase(1).state = PhaseState::Rejected;
        assert_eq!(ready(&plan.view()), vec![2, 3, 4]);
    }

    #[test]
    fn a_hard_dependency_must_be_accepted() {
        for integration in [Integration::Branch, Integration::None] {
            let mut plan = Plan::new(vec![
                node(1, json!({"state": "accepted", "integrated": true})),
                node(2, json!({"deps": [{"node": 1}]})),
            ]);
            plan.integration = integration;
            assert!(is_ready(&plan, 2), "{integration:?}");
            for state in [
                NodeState::Draft,
                NodeState::Waiting,
                NodeState::Pending,
                NodeState::InProgress,
                NodeState::Reported,
            ] {
                plan.node(1).state = state;
                assert!(!is_ready(&plan, 2), "{integration:?} {state:?}");
            }
        }
    }

    #[test]
    fn with_an_integration_branch_a_hard_dependency_must_also_be_merged() {
        let mut plan = Plan::new(vec![
            node(1, json!({"state": "accepted", "integrated": true})),
            node(2, json!({"deps": [{"node": 1}]})),
        ]);
        assert_eq!(plan.integration, Integration::Branch);
        assert!(is_ready(&plan, 2));
        plan.node(1).integrated = false;
        assert!(!is_ready(&plan, 2), "accepted but not merged");
    }

    #[test]
    fn without_an_integration_branch_an_accepted_dependency_releases() {
        // Nothing is merged in such a team, so nothing is recorded as merged.
        let mut plan = Plan::new(vec![
            node(1, json!({"state": "accepted"})),
            node(2, json!({"deps": [{"node": 1}]})),
        ]);
        assert!(!is_ready(&plan, 2), "a branch team waits for the merge");
        plan.integration = Integration::None;
        assert!(is_ready(&plan, 2));
    }

    #[test]
    fn a_stage_is_never_a_dependency() {
        // A dependency that names a stage holds, like one missing from the
        // view, whatever the stage's state and the team's integration.
        let mut plan = Plan::new(vec![
            node(1, json!({"state": "reported", "implementer": "m1"})),
            stage(3, 1, "m1"),
            node(4, json!({"deps": [{"node": 3}]})),
            node(5, json!({"deps": [{"node": 3, "kind": "soft"}]})),
        ]);
        let cases = [
            (NodeState::Accepted, true),
            (NodeState::Accepted, false),
            (NodeState::InProgress, false),
            (NodeState::Reported, false),
            (NodeState::Cancelled, false),
        ];
        for integration in [Integration::Branch, Integration::None] {
            plan.integration = integration;
            for (state, integrated) in cases {
                plan.node(3).state = state;
                plan.node(3).integrated = integrated;
                let found = ready(&plan.view());
                assert!(
                    !found.contains(&4) && !found.contains(&5),
                    "{integration:?} {state:?} {integrated}"
                );
            }
        }
        // The same dependency on a task releases.
        plan.node(3).kind = crate::model::NodeKind::Task;
        assert!(is_ready(&plan, 4) && is_ready(&plan, 5));
    }

    #[test]
    fn a_soft_dependency_must_have_started() {
        let mut plan = Plan::new(vec![
            node(1, json!({"state": "in_progress"})),
            node(2, json!({"deps": [{"node": 1, "kind": "soft"}]})),
        ]);
        assert!(is_ready(&plan, 2));
        for state in [NodeState::Reported, NodeState::Accepted] {
            plan.node(1).state = state;
            assert!(is_ready(&plan, 2), "{state:?}");
        }
        for state in [NodeState::Draft, NodeState::Waiting, NodeState::Pending] {
            plan.node(1).state = state;
            assert!(!is_ready(&plan, 2), "{state:?}");
        }
    }

    #[test]
    fn a_cancelled_dependency_releases() {
        let mut plan = Plan::new(vec![
            node(1, json!({"state": "cancelled"})),
            node(2, json!({"deps": [{"node": 1}]})),
            node(3, json!({"deps": [{"node": 1, "kind": "soft"}]})),
        ]);
        assert!(is_ready(&plan, 2) && is_ready(&plan, 3));
        plan.node(1).state = NodeState::Pending;
        assert!(!is_ready(&plan, 2) && !is_ready(&plan, 3));
    }

    #[test]
    fn a_dependency_missing_from_the_view_holds() {
        let mut plan = Plan::new(vec![
            node(1, json!({"state": "accepted", "integrated": true})),
            node(2, json!({"deps": [{"node": 1}]})),
        ]);
        assert!(is_ready(&plan, 2));
        plan.nodes.remove(0);
        assert!(!is_ready(&plan, 2));
    }

    #[test]
    fn an_open_task_writing_the_same_files_holds() {
        let mut plan = Plan::new(vec![
            node(1, json!({"state": "in_progress", "writes": ["src/teams"]})),
            node(2, json!({"writes": ["src/teams/ready.rs"]})),
        ]);
        assert!(!is_ready(&plan, 2));
        plan.node(1).state = NodeState::Reported;
        assert!(!is_ready(&plan, 2), "a reported result is not merged yet");
        plan.node(1).state = NodeState::Accepted;
        assert!(is_ready(&plan, 2));
        plan.node(1).state = NodeState::InProgress;
        plan.node(1).writes = vec!["docs".into()];
        assert!(is_ready(&plan, 2));
    }

    #[test]
    fn a_task_in_a_dependency_cycle_is_never_ready() {
        // 1 started before a later edit closed the cycle; 2's soft
        // dependency on it would otherwise be met.
        let mut plan = Plan::new(vec![
            node(
                1,
                json!({"state": "in_progress", "deps": [{"node": 2, "kind": "soft"}]}),
            ),
            node(2, json!({"deps": [{"node": 1, "kind": "soft"}]})),
        ]);
        assert!(!is_ready(&plan, 2));
        // A cancelled node closes no cycle.
        plan.node(1).state = NodeState::Cancelled;
        assert!(is_ready(&plan, 2));
    }

    #[test]
    fn a_stage_waits_for_its_task_to_be_reported() {
        let mut plan = Plan::new(vec![reported(1, "m1"), stage(2, 1, "m1")]);
        assert!(is_ready(&plan, 2));
        for state in [
            NodeState::InProgress,
            NodeState::Pending,
            NodeState::Accepted,
        ] {
            plan.node(1).state = state;
            assert!(!is_ready(&plan, 2), "{state:?}");
        }
        plan.node(1).state = NodeState::Reported;
        plan.node(2).stage_of = Some(9);
        assert!(!is_ready(&plan, 2), "an unknown task");
        plan.node(2).stage_of = None;
        assert!(!is_ready(&plan, 2), "no task");
    }

    #[test]
    fn a_node_listed_twice_is_never_ready() {
        let mut plan = Plan::new(vec![node(1, json!({})), node(2, json!({}))]);
        assert_eq!(given(&plan), pairs(&[(1, "m1"), (2, "m2")]));
        plan.nodes.push(node(1, json!({})));
        assert_eq!(ready(&plan.view()), vec![2]);
        assert_eq!(given(&plan), pairs(&[(2, "m1")]));
    }

    #[test]
    fn a_node_listed_twice_holds_what_depends_on_it() {
        let mut plan = Plan::new(vec![
            node(1, json!({"state": "accepted", "integrated": true})),
            node(2, json!({"deps": [{"node": 1}]})),
            node(3, json!({"deps": [{"node": 1, "kind": "soft"}]})),
        ]);
        assert!(is_ready(&plan, 2) && is_ready(&plan, 3));
        // Which row is true is unknown, so neither releases.
        plan.nodes.insert(0, node(1, json!({})));
        let found = ready(&plan.view());
        assert!(!found.contains(&2) && !found.contains(&3), "{found:?}");
    }

    #[test]
    fn a_stage_of_a_task_listed_twice_is_not_ready() {
        let mut plan = Plan::new(vec![
            reported(1, "m1"),
            node(
                5,
                json!({"kind": "stage", "stage_of": 1, "role": "reviewer"}),
            ),
        ]);
        plan.members.push(member("m4", "reviewer"));
        assert!(is_ready(&plan, 5));
        // Two rows name different implementers; either could be the reviewer.
        plan.nodes.push(reported(1, "m3"));
        assert!(!is_ready(&plan, 5));
        assert!(given(&plan).is_empty());
    }

    #[test]
    fn a_stage_of_an_earlier_round_is_not_ready() {
        let mut plan = Plan::new(vec![
            node(
                1,
                json!({"state": "reported", "implementer": "m1", "round": 2}),
            ),
            node(
                5,
                json!({"kind": "stage", "stage_of": 1, "role": "reviewer", "implementer": "m1", "round": 2}),
            ),
        ]);
        assert!(is_ready(&plan, 5));
        plan.node(5).round = 1;
        assert!(!is_ready(&plan, 5), "the round-1 stage is obsolete");
        plan.node(5).round = 3;
        assert!(!is_ready(&plan, 5));
    }

    // Hand-out.

    #[test]
    fn review_focus_2_two_ready_tasks_and_one_free_member_hand_out_one() {
        let mut plan = Plan::new(vec![node(1, json!({})), node(2, json!({}))]);
        // m2 has a task in progress outside the team, so it is not free.
        plan.member("m2").free = false;
        assert_eq!(ready(&plan.view()), vec![1, 2]);
        assert_eq!(given(&plan), pairs(&[(1, "m1")]));
    }

    #[test]
    fn stages_go_first_then_planned_tasks_then_the_queue_in_board_order() {
        let mut plan = Plan::new(vec![
            queued(10, "a2"),
            queued(12, "a1"),
            queued(11, "a1"),
            node(1, json!({})),
            node(2, json!({})),
            node(3, json!({"deps": [{"node": 2}]})),
            reported(4, "m1"),
            stage(20, 4, "m1"),
        ]);
        // Equal ranks go by task UUID, which here runs against the node IDs.
        plan.node(12).task.as_mut().unwrap().uuid = "00000000-0000-4000-8000-000000000000".into();
        plan.members = vec![member(LEAD, LEAD), member("r1", "reviewer")];
        for n in 1..=6 {
            plan.members.push(member(&format!("d{n}"), "developer"));
        }
        // 2 unblocks 3, so it goes before 1.
        assert_eq!(ready(&plan.view()), vec![20, 2, 1, 12, 11, 10]);
        assert_eq!(
            given(&plan),
            pairs(&[
                (20, "r1"),
                (2, "d1"),
                (1, "d2"),
                (12, "d3"),
                (11, "d4"),
                (10, "d5")
            ])
        );
    }

    #[test]
    fn only_an_active_free_member_of_the_role_takes_a_task() {
        let mut plan = Plan::new(vec![node(1, json!({}))]);
        plan.members = vec![member(LEAD, "developer"), member("m1", "developer")];
        assert_eq!(given(&plan), pairs(&[(1, "m1")]));
        for state in [
            MemberState::NotCreated,
            MemberState::Paused,
            MemberState::Removed,
        ] {
            plan.member("m1").state = state;
            assert!(
                given(&plan).is_empty(),
                "{state:?}; the lead never takes one"
            );
        }
        plan.member("m1").state = MemberState::Active;
        plan.member("m1").free = false;
        assert!(given(&plan).is_empty());
        plan.member("m1").free = true;
        plan.member("m1").role = "reviewer".into();
        assert!(given(&plan).is_empty());
    }

    #[test]
    fn a_named_assignee_is_the_only_member_who_takes_the_task() {
        let mut plan = Plan::new(vec![node(1, json!({"assignee": "m2"}))]);
        assert_eq!(given(&plan), pairs(&[(1, "m2")]));
        plan.member("m2").free = false;
        assert!(given(&plan).is_empty(), "m1 is free but not the assignee");
    }

    #[test]
    fn each_member_takes_one_task_per_call() {
        let plan = Plan::new(vec![
            node(1, json!({})),
            node(2, json!({})),
            node(3, json!({})),
        ]);
        assert_eq!(given(&plan), pairs(&[(1, "m1"), (2, "m2")]));
    }

    #[test]
    fn two_tasks_writing_the_same_files_are_not_handed_out_together() {
        let plan = Plan::new(vec![
            node(1, json!({"writes": ["src/a.rs"]})),
            node(2, json!({"writes": ["src/a.rs"]})),
            node(3, json!({"writes": ["src/b.rs"]})),
        ]);
        assert_eq!(ready(&plan.view()), vec![1, 2, 3]);
        assert_eq!(given(&plan), pairs(&[(1, "m1"), (3, "m2")]));
    }

    #[test]
    fn a_member_holding_a_task_in_progress_is_not_given_another() {
        let mut plan = Plan::new(vec![
            node(1, json!({"state": "in_progress", "implementer": "m1"})),
            node(2, json!({})),
        ]);
        plan.member("m2").free = false;
        // m1 still reads as free, as it may before the caller sees the start.
        assert!(given(&plan).is_empty());
        plan.node(1).implementer = None;
        plan.node(1).assignee = Some("m1".into());
        assert!(given(&plan).is_empty());
        plan.node(1).state = NodeState::Reported;
        assert_eq!(given(&plan), pairs(&[(2, "m1")]));
    }

    #[test]
    fn review_focus_4_a_stage_is_never_handed_to_its_implementer() {
        // The reviewer m3 implemented task 1 and is the only reviewer.
        let mut plan = Plan::new(vec![
            node(
                1,
                json!({"state": "reported", "role": "reviewer", "implementer": "m3"}),
            ),
            stage(2, 1, "m3"),
        ]);
        assert_eq!(ready(&plan.view()), vec![2]);
        assert!(given(&plan).is_empty());
        let stuck = unstaffed(&plan.view(), &BTreeSet::new());
        assert_eq!(stuck.len(), 1);
        assert_eq!(stuck[0].node, 2);
        assert_eq!(
            stuck[0].reason,
            "m3 implemented the task and is the only member with the role reviewer, \
             and a stage is never handed to a member who implemented any round of the task. \
             The task went to the lead, who can accept, return or cancel it there."
        );
        // Either record naming the implementer is enough.
        plan.node(1).implementer = None;
        assert!(given(&plan).is_empty());
        assert_eq!(unstaffed(&plan.view(), &BTreeSet::new()).len(), 1);
        plan.node(1).implementer = Some("m3".into());
        plan.node(2).implementer = None;
        assert!(given(&plan).is_empty());
        assert_eq!(unstaffed(&plan.view(), &BTreeSet::new()).len(), 1);
        // A second reviewer takes it.
        plan.members.push(member("m4", "reviewer"));
        assert_eq!(given(&plan), pairs(&[(2, "m4")]));
        assert!(unstaffed(&plan.view(), &BTreeSet::new()).is_empty());
    }

    #[test]
    fn unstaffed_names_ready_tasks_no_member_can_take() {
        let mut plan = Plan::new(vec![
            node(1, json!({"role": "tester"})),
            node(2, json!({"assignee": "m9"})),
            node(3, json!({"role": "tester", "deps": [{"node": 9}]})),
            node(4, json!({})),
        ]);
        // m1 and m2 are busy elsewhere: busy is not unstaffed.
        plan.member("m1").free = false;
        plan.member("m2").free = false;
        let stuck = unstaffed(&plan.view(), &BTreeSet::new());
        let ids: Vec<NodeId> = stuck.iter().map(|item| item.node).collect();
        assert_eq!(ids, vec![1, 2], "3 is not ready, 4 waits for a member");
        assert!(stuck[0].reason.contains("tester"), "{}", stuck[0].reason);
        assert!(stuck[1].reason.contains("m9"), "{}", stuck[1].reason);
        // A removed member counts for nothing; a paused one may come back.
        plan.member("m1").state = MemberState::Removed;
        plan.member("m2").state = MemberState::Paused;
        assert_eq!(unstaffed(&plan.view(), &BTreeSet::new()).len(), 2);
        plan.member("m2").state = MemberState::Removed;
        assert_eq!(unstaffed(&plan.view(), &BTreeSet::new()).len(), 3);
    }

    #[test]
    fn the_shared_fixture_hands_out_the_review_and_the_next_task() {
        let team: TeamFixture =
            serde_json::from_str(include_str!("../fixtures/team-queue.json")).unwrap();
        let members: Vec<MemberView> = team
            .members
            .iter()
            .map(|member| MemberView {
                id: member.id.clone(),
                role: member.role.clone(),
                state: member.state,
                free: true,
            })
            .collect();
        let view = PlanView {
            phases: &team.phases,
            nodes: &team.nodes,
            members: &members,
            lead: &team.team.lead,
            integration: team.team.settings.integration,
        };
        // 5 reviews 2; 4 is next in the queue; 6 is not admitted; m2 holds 3.
        assert_eq!(ready(&view), vec![5, 4]);
        assert_eq!(
            handouts(&view),
            vec![
                Handout {
                    node: 5,
                    member: "m3".into()
                },
                Handout {
                    node: 4,
                    member: "m1".into()
                },
            ]
        );
        assert!(unstaffed(&view, &BTreeSet::new()).is_empty());
    }

    #[test]
    fn every_implementer_rounds_reason_names_the_rule_and_the_way_forward() {
        // zerus-63rru.20 round 2: the kernel's gap reason names every round and
        // the way forward when every member with the role implemented.
        use crate::ready::{MemberView, PlanView, Staffing, Work};
        use crate::{
            Member, MemberState, Node, NodeKind, NodeSource, NodeState as NState, Phase, PhaseKind,
            PhaseState, Severity,
        };
        let node = Node {
            id: 2,
            phase: 0,
            task: None,
            title: "Review".into(),
            text: String::new(),
            source: NodeSource::Sent,
            deps: vec![],
            assignee: None,
            role: "developer".into(),
            pipeline: String::new(),
            writes: vec![],
            estimate_minutes: 0,
            kind: NodeKind::Stage,
            severity: Severity::None,
            origin: None,
            stage_of: Some(1),
            stage_index: 0,
            round: 1,
            implementer: None,
            earlier_implementers: vec!["m1".into()],
            hold: false,
            admitted: true,
            state: NState::Pending,
            decided_by: None,
            integrated: false,
            rank: String::new(),
        };
        let members = [Member {
            id: "m1".into(),
            role: "developer".into(),
            agent: "claude".into(),
            launch: None,
            account: String::new(),
            state: MemberState::Active,
            session: None,
            worktree: String::new(),
            ownership_nonce: String::new(),
        }];
        let member_views: Vec<MemberView> = members
            .iter()
            .map(|m| MemberView {
                id: m.id.clone(),
                role: m.role.clone(),
                state: m.state,
                free: false,
            })
            .collect();
        let phases = vec![Phase {
            n: 0,
            order: 0,
            title: String::new(),
            kind: PhaseKind::Queue,
            mode: crate::PhaseMode::default(),
            approval: crate::Approval::default(),
            state: PhaseState::Approved,
            acceptance: crate::Acceptance::default(),
            barrier: false,
            paused: false,
        }];
        let nodes = vec![node.clone()];
        let lead = String::from("lead");
        let view = PlanView {
            phases: &phases,
            nodes: &nodes,
            members: &member_views,
            lead: &lead,
            integration: crate::Integration::None,
        };
        let work = Work {
            role: "developer",
            assignee: None,
            implementers: vec!["m1"],
        };
        let reason = Staffing { view }.gap(&work).unwrap();
        assert!(
            reason.contains("any round") && reason.contains("accept, return or cancel"),
            "{reason}"
        );
    }
}
