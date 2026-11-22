//! The Team tab's view: the bar, the member cards and the queue as the board's
//! groups, each card with its stages.
use super::*;

/// The member counts of `counts.members`.
pub(super) fn team_counts(value: &Value) -> TeamCounts {
    let members = &value["members"];
    let nodes = &value["nodes"];
    TeamCounts {
        members: MemberCounts {
            total: count(members, "total"),
            not_created: count(members, "not_created"),
            active: count(members, "active"),
            paused: count(members, "paused"),
            removed: count(members, "removed"),
        },
        nodes: NodeCounts {
            waiting: count(nodes, "waiting"),
            pending: count(nodes, "pending"),
            in_progress: count(nodes, "in_progress"),
            reported: count(nodes, "reported"),
            accepted: count(nodes, "accepted"),
            cancelled: count(nodes, "cancelled"),
        },
        inbox: count(&value["inbox"], "open"),
        cards: count(&value["cards"], "open"),
    }
}

fn member_cards(status: &Value) -> Vec<MemberCard> {
    let lead = name(&status["team"], "lead");
    array(status, "members")
        .iter()
        .take(MEMBERS_KEPT)
        .map(|member| MemberCard {
            id: name(member, "id"),
            role: name(member, "role"),
            agent: name(member, "agent"),
            account: name(member, "account"),
            state: or_default(member, "state", "not_created"),
            lead: member.get("id").and_then(Value::as_str) == Some(lead.as_str()),
            session: session(member),
            model: optional_name(member, "model"),
            person_stopped: flag(member, "person_stopped"),
            effort: optional_name(member, "effort"),
            account_choice: super::editing::choice(member),
            open_work: optional_count(member, "open_work"),
        })
        .collect()
}

/// A stage row: who does the stage (`assignee`) and who implemented the task
/// it checks (`implementer`, the plan's binding decision).
fn stage_card(node: &Value) -> StageCard {
    // A stage row names its kind, its target and that target's round in its own
    // `stage` object; an older command line omits the object, and a kind the
    // core does not know is kept out as absent rather than refused.
    let stage = node.get("stage").filter(|stage| stage.is_object());
    StageCard {
        node: count(node, "id"),
        index: count(node, "stage_index"),
        role: name(node, "role"),
        state: or_default(node, "state", "draft"),
        assignee: name(node, "assignee"),
        holder: node
            .get("holder")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.chars().take(NAME_CHARS + 1).count() <= NAME_CHARS)
            .map(str::to_owned),
        implementer: name(node, "implementer"),
        kind: stage.and_then(stage_kind),
        target: stage.and_then(|stage| optional_count(stage, "target")),
        round: stage.and_then(|stage| optional_count(stage, "round")),
    }
}

/// Whether a node is a pipeline stage: it checks its target instead of being
/// a board card of its own.
fn is_stage(node: &Value) -> bool {
    name(node, "kind") == "stage" || node.get("stage_of").and_then(Value::as_u64).is_some()
}

fn card(node: &Value) -> QueueCard {
    QueueCard {
        assigned_base: optional_name(node, "assigned_base"),
        deps_editable: node.get("deps_editable").and_then(Value::as_bool),
        node: count(node, "id"),
        deps: graph::dependencies(node),
        phase: node
            .get("phase")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0),
        origin: node
            .get("origin")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        layer: None,
        title: text(node, "title", LINE_CHARS),
        state: or_default(node, "state", "draft"),
        task: task_ref(node),
        role: name(node, "role"),
        pipeline: name(node, "pipeline"),
        assignee: name(node, "assignee"),
        implementer: name(node, "implementer"),
        earlier_implementers: earlier_implementers(node),
        rank: name(node, "rank"),
        round: count(node, "round"),
        kind: row_kind(node),
        rounds: optional_count(node, "rounds"),
        held: flag(node, "hold"),
        admitted: flag(node, "admitted"),
        integrated: flag(node, "integrated"),
        decided_by: name(node, "decided_by"),
        stages: Vec::new(),
        holder: holder(node),
        pending_decision: optional_name(node, "pending_decision"),
        pending_replaceable: flag(node, "pending_replaceable"),
        stop_outcome: node.get("stop_outcome").filter(|v| v.is_object()).map(|v| {
            crate::upstream_teams::view::StopOutcomeCard {
                code: name(v, "code"),
                reason: v.get("reason").and_then(Value::as_str).map(str::to_owned),
            }
        }),
    }
}

/// A stage row's earlier implementers of its target: valid member names
/// only, at most 200 (zerus-3u5yu).
fn earlier_implementers(node: &Value) -> Vec<String> {
    node.get("earlier_implementers")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .filter(|id| {
                    !id.is_empty() && id.chars().take(NAME_CHARS + 1).count() <= NAME_CHARS
                })
                .take(200)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Folds the stage rows into the card they check, keeping every group in
/// [`GROUPS`]' order and the rest by state name. A stage whose target is
/// outside the page stays a card of its own, so its work is not lost; its
/// target's card may come with a later page. That card's `stages` holds its
/// own stage facts (kind, target, round), which the queue card has no field
/// for.
fn fold_stages(nodes: &[Value]) -> (Vec<QueueCard>, bool) {
    let mut cards: Vec<QueueCard> = Vec::new();
    let mut stages: Vec<(u64, StageCard, QueueCard)> = Vec::new();
    let mut dropped = false;
    let mut cards_seen = 0usize;
    for node in nodes {
        if is_stage(node) {
            stages.push((count(node, "stage_of"), stage_card(node), card(node)));
            continue;
        }
        cards_seen += 1;
        if cards.len() >= PAGE {
            dropped = true;
            continue;
        }
        cards.push(card(node));
    }
    for (target, stage, mut own_card) in stages {
        let Some(card) = cards.iter_mut().find(|card| card.node == target) else {
            if cards.len() < PAGE {
                own_card.stages.push(stage);
                cards.push(own_card);
            } else {
                dropped = true;
            }
            continue;
        };
        if card.stages.len() < STAGES_KEPT {
            card.stages.push(stage);
        } else {
            dropped = true;
        }
    }
    (cards, dropped || cards_seen > PAGE)
}

/// Groups the cards by state; every group of [`GROUPS`] is present.
fn group_by_state(mut cards: Vec<QueueCard>) -> Vec<BoardGroup> {
    let mut by_state: std::collections::BTreeMap<String, Vec<QueueCard>> = Default::default();
    for card in cards.drain(..) {
        by_state.entry(card.state.clone()).or_default().push(card);
    }
    for cards in by_state.values_mut() {
        cards.sort_by(|a, b| a.rank.cmp(&b.rank).then(a.node.cmp(&b.node)));
        for card in cards.iter_mut() {
            card.stages
                .sort_by(|a, b| a.index.cmp(&b.index).then(a.node.cmp(&b.node)));
        }
    }
    let mut order: Vec<String> = GROUPS.iter().map(|state| (*state).to_owned()).collect();
    order.extend(
        by_state
            .keys()
            .filter(|state| !GROUPS.contains(&state.as_str()))
            .cloned(),
    );
    order
        .into_iter()
        .map(|state| {
            let cards = by_state.remove(&state).unwrap_or_default();
            BoardGroup { state, cards }
        })
        .collect()
}

/// The queue as the board's groups.
pub(super) fn board(nodes: &[Value]) -> (Vec<BoardGroup>, bool) {
    let (mut cards, truncated) = fold_stages(nodes);
    graph::layers(&mut cards);
    (group_by_state(cards), truncated)
}

/// The Team tab's view from a status reply (the team, its members and counts)
/// and a queue reply (one page of nodes).
pub fn team(status: &Value, queue: &Value) -> TeamView {
    let team = &status["team"];
    let (groups, truncated) = board(array(queue, "rows"));
    let revision = count(team, "revision");
    TeamView {
        planning: super::planning::project(status),
        plan: None,
        bar: TeamBar {
            id: name(team, "id"),
            project: name(team, "project"),
            name: name(team, "name"),
            status: name(team, "status"),
            goal: text(team, "goal", TEXT_CHARS),
            lead: name(team, "lead"),
            integration_branch: name(team, "integration_branch"),
            base_commit: name(team, "base_commit"),
            revision,
            counts: team_counts(&status["counts"]),
        },
        settings: crate::upstream_teams::settings::complete(&team["settings"]),
        members: member_cards(status),
        member_roles: super::editing::roles(status),
        routing_pipelines: routing_pipelines(team),
        edit_goal: super::editing::goal(status),
        groups,
        truncated,
        next: next_cursor(queue),
        revision,
        after: None,
    }
}

fn routing_pipelines(team: &Value) -> Vec<RoutingPipeline> {
    let bundle = &team["bundle"];
    let roles = array(bundle, "roles")
        .iter()
        .take(MEMBERS_KEPT)
        .filter_map(|role| role.get("id")?.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    array(bundle, "pipelines")
        .iter()
        .take(MEMBERS_KEPT)
        .filter_map(|pipeline| {
            let id = name(pipeline, "id");
            let role = name(pipeline, "implementer");
            if zerus_team_plan::id_problem(&id, "pipeline").is_some()
                || !roles.contains(role.as_str())
            {
                return None;
            }
            Some(RoutingPipeline {
                id,
                role,
                name: text(pipeline, "name", LINE_CHARS),
            })
        })
        .collect()
}
