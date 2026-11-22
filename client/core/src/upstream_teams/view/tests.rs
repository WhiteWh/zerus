use super::*;
use serde_json::json;

/// The pinned CLI replies of package 5. The list reply is the command line's
/// own `crates/team-plan/fixtures/cli-list.json`, regenerated with
/// `member_sessions`. The others are copies beside this module, the same
/// bytes as `crates/team-plan/fixtures/cli-*.json` at its `fa46dcc08`: they
/// keep the shape without the later additive fields, which the core still
/// reads.
fn fixture(name: &str) -> Value {
    let text = match name {
        "status" => include_str!("../fixtures/cli-status.json"),
        "queue" => include_str!("../fixtures/cli-queue.json"),
        "cards" => include_str!("../fixtures/cli-cards.json"),
        "waiting" => include_str!("../fixtures/cli-waiting.json"),
        "report-notes-card" => include_str!("../fixtures/report-notes-card.json"),
        "list" => include_str!("../../../../../crates/team-plan/fixtures/cli-list.json"),
        other => panic!("no fixture {other}"),
    };
    serde_json::from_str(text).expect("a pinned CLI fixture parses")
}

const TEAM: &str = "00000000-0000-4000-8000-00000000beef";

fn group<'a>(view: &'a TeamView, state: &str) -> &'a BoardGroup {
    view.groups
        .iter()
        .find(|group| group.state == state)
        .expect("the board carries every group")
}

#[test]
fn the_pinned_status_and_queue_project_the_team() {
    let view = team(&fixture("status"), &fixture("queue"));
    assert_eq!(view.bar.id, TEAM);
    assert_eq!(view.bar.name, "Fixes");
    assert_eq!(view.bar.project, "example-project");
    assert_eq!(view.bar.status, "running");
    assert_eq!(view.bar.lead, "lead");
    assert_eq!(view.bar.goal, "");
    assert_eq!(view.bar.integration_branch, "zerus/team/fixes");
    assert_eq!(
        view.bar.base_commit,
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert_eq!(view.bar.revision, 16);
    assert_eq!(view.revision, 16);
    assert!(!view.truncated);
    assert_eq!(view.next, None);
    assert_eq!(
        view.bar.counts,
        TeamCounts {
            members: MemberCounts {
                total: 4,
                not_created: 2,
                active: 2,
                paused: 0,
                removed: 0,
            },
            nodes: NodeCounts {
                waiting: 0,
                pending: 1,
                in_progress: 0,
                reported: 1,
                accepted: 0,
                cancelled: 0,
            },
            inbox: 1,
            cards: 1,
        }
    );

    // The members: two launched and two still not created.
    assert_eq!(
        view.members
            .iter()
            .map(|member| member.id.as_str())
            .collect::<Vec<_>>(),
        ["lead", "m1", "m2", "m3"]
    );
    assert_eq!(view.members[0].state, "active");
    assert!(view.members[0].lead);
    assert!(!view.members[1].lead);
    assert_eq!(view.members[1].role, "developer");
    assert_eq!(
        view.members[1].session,
        Some(SessionRef {
            machine: "00000000-0000-4000-8000-0000000000aa".into(),
            session_id: "launch_id:00000000-0000-4000-8000-000000000031".into(),
        })
    );
    assert_eq!(
        view.members[2].state, "not_created",
        "the CLI omits a default"
    );
    assert_eq!(view.members[2].account, "");
    assert_eq!(view.members[2].session, None);

    // Every group, in their order.
    assert_eq!(
        view.groups
            .iter()
            .map(|group| group.state.as_str())
            .collect::<Vec<_>>(),
        GROUPS
    );

    let pending = group(&view, "pending");
    assert_eq!(pending.cards.len(), 1);
    assert_eq!(pending.cards[0].node, 1);
    assert_eq!(pending.cards[0].title, "Task 1");
    assert_eq!(
        pending.cards[0].task,
        Some(TaskRefView {
            project: "example-project".into(),
            uuid: "00000000-0000-4000-8000-000000000001".into(),
        })
    );
    assert!(pending.cards[0].admitted);
    assert_eq!(pending.cards[0].round, 1);
    assert_eq!(pending.cards[0].rank, "a01");

    let reported = group(&view, "reported");
    assert_eq!(reported.cards.len(), 1);
    assert_eq!(reported.cards[0].node, 2);
    assert_eq!(reported.cards[0].implementer, "m1");
    assert!(reported.cards[0].stages.is_empty());
}

#[test]
fn a_card_shows_a_stage_in_each_state() {
    let mut rows = vec![serde_json::json!({
        "id": 1, "state": "reported", "title": "Fix it", "rank": "a0",
        "task": {"project": "p", "uuid": "u"},
    })];
    for (at, state) in [
        "waiting",
        "pending",
        "in_progress",
        "reported",
        "accepted",
        "cancelled",
    ]
    .into_iter()
    .enumerate()
    {
        rows.push(serde_json::json!({
            "id": at as u64 + 2, "state": state, "kind": "stage",
            "stage_of": 1, "stage_index": at as u64, "role": "reviewer",
        }));
    }
    let queue = serde_json::json!({"ok": true, "team": TEAM, "rows": rows, "next": null});
    let view = team(&fixture("status"), &queue);
    let card = &group(&view, "reported").cards[0];
    assert_eq!(card.stages.len(), 6);
    assert_eq!(
        card.stages
            .iter()
            .map(|stage| stage.state.as_str())
            .collect::<Vec<_>>(),
        [
            "waiting",
            "pending",
            "in_progress",
            "reported",
            "accepted",
            "cancelled"
        ]
    );
    assert_eq!(
        card.stages
            .iter()
            .map(|stage| stage.index)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3, 4, 5]
    );
}

#[test]
fn an_empty_queue_has_every_group_and_no_card() {
    let queue = serde_json::json!({"ok": true, "team": TEAM, "rows": [], "next": null});
    let view = team(&fixture("status"), &queue);
    assert_eq!(view.groups.len(), GROUPS.len());
    assert!(view.groups.iter().all(|group| group.cards.is_empty()));
    assert!(!view.truncated);
    assert_eq!(view.next, None);
}

#[test]
fn the_board_keeps_one_page_and_says_so() {
    let rows: Vec<Value> = (1..=250)
        .map(|id| {
            serde_json::json!({
                "id": id, "state": "pending", "title": format!("Task {id}"),
                "rank": format!("a{id:03}"),
            })
        })
        .collect();
    let queue = serde_json::json!({"ok": true, "team": TEAM, "rows": rows,
        "next": "t1.bmV4dA"});
    let view = team(&fixture("status"), &queue);
    let cards = view.groups.iter().flat_map(|group| &group.cards).count();
    assert_eq!(cards, PAGE, "the view keeps one page");
    assert!(view.truncated);
    assert_eq!(
        view.next.as_deref(),
        Some("t1.bmV4dA"),
        "the next page's cursor is visible"
    );
}

#[test]
fn a_draft_node_without_a_state_is_grouped_as_a_draft() {
    let queue = serde_json::json!({"ok": true, "team": TEAM, "rows": [
        {"id": 9, "title": "Draw it up", "rank": "a0"},
    ], "next": null});
    let view = team(&fixture("status"), &queue);
    assert_eq!(group(&view, "draft").cards[0].node, 9);
}

#[test]
fn long_texts_are_cut() {
    let long = "x".repeat(TEXT_CHARS + 100);
    let mut status = fixture("status");
    status["team"]["goal"] = serde_json::json!(long);
    let queue = serde_json::json!({"ok": true, "team": TEAM, "rows": [
        {"id": 1, "title": long, "state": "pending"},
    ], "next": null});
    let view = team(&status, &queue);
    assert_eq!(view.bar.goal.chars().count(), TEXT_CHARS);
    assert_eq!(
        group(&view, "pending").cards[0].title.chars().count(),
        LINE_CHARS
    );
}

#[test]
fn a_stage_shows_its_own_worker_not_the_task_implementer() {
    // The wave-2 binding decision keeps a stage's `implementer` as the
    // person who implemented the task being checked; the stage's own
    // worker is its `assignee`.
    let queue = serde_json::json!({"ok": true, "team": TEAM, "rows": [
        {"id": 1, "state": "reported"},
        {"id": 2, "kind": "stage", "stage_of": 1, "state": "in_progress",
         "assignee": "reviewer", "implementer": "developer"},
    ], "next": null});
    let view = team(&fixture("status"), &queue);
    let card = view
        .groups
        .iter()
        .flat_map(|group| &group.cards)
        .find(|card| card.node == 1)
        .expect("the checked card");
    assert_eq!(card.stages[0].assignee, "reviewer");
    assert_eq!(card.stages[0].implementer, "developer");
}

#[test]
fn an_unknown_state_and_a_huge_revision_do_not_panic() {
    let mut status = fixture("status");
    status["team"]["status"] = serde_json::json!("future_state");
    status["team"]["revision"] = serde_json::json!(1e100);
    let queue = serde_json::json!({"ok": true, "team": TEAM, "rows": [
        {"id": 1, "state": "future_state", "title": "new", "round": -2},
    ], "next": null});
    let view = team(&status, &queue);
    assert_eq!(view.bar.status, "future_state");
    assert_eq!(view.revision, u64::MAX);
    assert!(view
        .groups
        .iter()
        .any(|group| group.state == "future_state" && group.cards.len() == 1));
    assert!(team(&Value::Null, &Value::Null).members.is_empty());
}

#[test]
fn a_stage_without_its_card_stays_a_card() {
    let queue = serde_json::json!({"ok": true, "team": TEAM, "rows": [
        {"id": 7, "state": "reported", "kind": "stage", "stage_of": 99,
         "stage_index": 0, "role": "reviewer", "title": "Review"},
    ], "next": null});
    let view = team(&fixture("status"), &queue);
    let card = &group(&view, "reported").cards[0];
    assert_eq!(card.node, 7);
    assert_eq!(card.title, "Review");
    // The stage checks a card this page does not carry: it stays a card of
    // its own, and its `stages` holds only its own stage facts.
    assert_eq!(
        card.stages
            .iter()
            .map(|stage| stage.node)
            .collect::<Vec<_>>(),
        vec![7]
    );
}

#[test]
fn the_list_projects_the_teams_of_the_computer() {
    let list = list(&serde_json::json!({
        "ok": true,
        "actor": {"kind": "person"},
        "rows": [
            {"id": TEAM, "project": "example-project", "name": "Fixes",
             "status": "running", "lead": "lead",
             "home": "00000000-0000-4000-8000-0000000000aa", "revision": 16},
            {"id": "00000000-0000-4000-8000-00000000dead", "project": "example-project",
             "name": "Old", "status": "stopped", "lead": "lead",
             "home": "00000000-0000-4000-8000-0000000000aa", "revision": 4},
        ],
        "next": "t1.bmV4dA",
    }));
    assert_eq!(list.teams.len(), 2);
    assert!(!list.truncated);
    assert_eq!(list.next.as_deref(), Some("t1.bmV4dA"));
    assert_eq!(list.teams[0].id, TEAM);
    assert_eq!(list.teams[0].name, "Fixes");
    assert_eq!(list.teams[0].status, "running");
    assert_eq!(list.teams[0].lead, "lead");
    assert_eq!(list.teams[0].revision, 16);
    assert!(!list.teams[0].draft());
    assert_eq!(list.teams[1].status, "stopped");
}

#[test]
fn waiting_projects_the_open_owner_cards() {
    // The `waiting` read is the cards of every team, each row carrying its
    // team; the pinned cards reply gives the row.
    let cards = fixture("cards");
    let rows: Vec<Value> = cards["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let mut row = row.clone();
            row["team"] = serde_json::json!(TEAM);
            row
        })
        .collect();
    let view = waiting(&serde_json::json!({
        "ok": true,
        "actor": {"kind": "person"},
        "rows": rows,
        "next": null,
    }));
    assert_eq!(view.items.len(), 1);
    assert_eq!(view.next, None);
    let item = &view.items[0];
    assert_eq!(item.id, format!("{TEAM}:1"));
    assert_eq!(item.card, 1);
    assert_eq!(item.kind, "note");
    assert_eq!(item.text, "Tell the team the release moved to Monday.");
    assert_eq!(item.state, "open");
    assert_eq!(item.by, "local");
    assert_eq!(item.node, None);
    assert_eq!(item.created_ms, 1700000000000);
}

/// The card shape is the production writer's own: `report-notes-card.json`
/// is the person's `waiting` row for the pipeline acceptance card that
/// `talk::card_notes` wrote, kept equal to that writer by hgs's fixture test.
/// The service's notes are carried beside the card text, never inside it; an
/// older reply without them reads as no notes.
#[test]
fn waiting_carries_the_service_notes_apart_from_the_card_text() {
    // The fixture is the production `waiting` row; the view reads it in the
    // one page the read carries.
    let row = fixture("report-notes-card");
    let one_page = |row: Value| {
        json!({
            "ok": true,
            "actor": {"kind": "person"},
            "rows": [row],
            "next": null
        })
    };
    let view = waiting(&one_page(row.clone()));
    let item = &view.items[0];
    assert_eq!(item.kind, "pipeline_acceptance");
    assert_eq!(
        item.text,
        "Trimmed the label and added a case for an empty one."
    );
    assert_eq!(item.text, row["text"].as_str().unwrap());
    assert_eq!(
        item.notes,
        vec![
            ServiceNote {
                code: "text_shortened".into(),
                text: None,
                parameters: Vec::new(),
            },
            ServiceNote {
                code: "worktree_dirty".into(),
                text: None,
                parameters: Vec::new(),
            }
        ]
    );
    assert!(!item.text.contains("shortened"));
    let wire = serde_json::to_value(item).unwrap();
    assert_eq!(
        wire["notes"],
        json!([
            {"code": "text_shortened"},
            {"code": "worktree_dirty"}
        ])
    );

    // A row from before the notes column reads as no notes.
    let mut without_notes = row.clone();
    without_notes.as_object_mut().unwrap().remove("notes");
    assert!(waiting(&one_page(without_notes)).items[0].notes.is_empty());

    // The reader refuses an unknown-code case and a repeat, and keeps an
    // unknown code's own sentence; only the notes change, not the row shape.
    let mut older = row;
    older["notes"] = json!([
        {"code": "text_shortened"},
        {"code": "worktree_dirty"},
        {"code": "Worktree_dirty"},
        {"code": "worktree_dirty"},
        {"code": "other_note", "text": "A service sentence."}
    ]);
    assert_eq!(
        waiting(&one_page(older)).items[0].notes,
        vec![
            ServiceNote {
                code: "text_shortened".into(),
                text: None,
                parameters: Vec::new(),
            },
            ServiceNote {
                code: "worktree_dirty".into(),
                text: None,
                parameters: Vec::new(),
            },
            ServiceNote {
                code: "other_note".into(),
                text: Some("A service sentence.".into()),
                parameters: Vec::new(),
            }
        ]
    );
}

#[test]
fn waiting_keeps_one_page() {
    let rows: Vec<Value> = (1..=250)
        .map(|seq| serde_json::json!({"team": TEAM, "seq": seq, "state": "open"}))
        .collect();
    let view = waiting(&serde_json::json!({
        "ok": true,
        "actor": {"kind": "person"},
        "rows": rows,
        "next": "t1.bmV4dA",
    }));
    assert_eq!(view.items.len(), PAGE);
    assert_eq!(view.next.as_deref(), Some("t1.bmV4dA"));
}

#[test]
fn the_pinned_waiting_reply_projects_the_open_owner_card() {
    let view = waiting(&fixture("waiting"));
    assert_eq!(view.next, None);
    assert_eq!(view.items.len(), 1);
    let item = &view.items[0];
    assert_eq!(item.id, "00000000-0000-4000-8000-00000000beef:1");
    assert_eq!(item.team, TEAM);
    assert_eq!(item.card, 1);
    assert_eq!(item.kind, "note");
    assert_eq!(item.node, None);
    assert_eq!(item.text, "Tell the team the release moved to Monday.");
    assert_eq!(item.state, "open");
    assert_eq!(item.by, "local");
    assert_eq!(item.created_ms, 1_700_000_000_000);
}

/// One hand-written queue row in the shape package 9 will publish with the
/// additive fields; until it regenerates `cli-queue.json` this keeps the
/// client core's parsing tested against the contract.
fn queue_row(id: u64, extra: Value) -> Value {
    let mut row =
        serde_json::json!({"id": id, "state": "pending", "role": "developer", "rank": "a01"});
    if let (Some(row), Some(extra)) = (row.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            row.insert(key.clone(), value.clone());
        }
    }
    row
}

fn status_with(members: Value) -> Value {
    serde_json::json!({
        "ok": true,
        "team": {"id": TEAM, "project": "example-project", "name": "Fixes", "status": "running", "lead": "lead", "revision": 16},
        "members": members,
        "counts": {"members": {}, "nodes": {}, "inbox": {"open": 0}, "cards": {"open": 0}}
    })
}

fn queue_with(rows: Value) -> Value {
    serde_json::json!({"ok": true, "rows": rows})
}

#[test]
fn a_stage_row_carries_its_kind_its_target_and_the_target_round() {
    let queue = queue_with(serde_json::json!([
        queue_row(
            1,
            serde_json::json!({"kind": "task", "rounds": 3, "title": "Task 1"})
        ),
        queue_row(
            7,
            serde_json::json!({"kind": "stage", "stage_of": 1, "stage_index": 0, "state": "pending",
            "stage": {"kind": "review", "target": 1, "round": 2}})
        ),
        queue_row(
            8,
            serde_json::json!({"kind": "stage", "stage_of": 1, "stage_index": 1, "state": "waiting",
            "stage": {"kind": "land", "target": 1, "round": 2}})
        ),
    ]));
    let status = status_with(serde_json::json!([
        {"id": "lead", "role": "lead", "agent": "claude", "state": "active", "model": "sonnet-5"}
    ]));
    let view = team(&status, &queue);
    let card = &view
        .groups
        .iter()
        .find(|group| group.state == "pending")
        .expect("the pending group carries the card")
        .cards[0];
    assert_eq!(card.kind, Some(RowKind::Task));
    assert_eq!(card.rounds, Some(3));
    assert_eq!(card.stages.len(), 2);
    assert_eq!(card.stages[0].kind, Some(StageKind::Review));
    assert_eq!(card.stages[0].target, Some(1));
    assert_eq!(card.stages[0].round, Some(2));
    assert_eq!(card.stages[1].kind, Some(StageKind::Land));
    assert_eq!(card.stages[1].target, Some(1));
    assert_eq!(view.members[0].model.as_deref(), Some("sonnet-5"));
}

#[test]
fn an_unknown_stage_kind_is_absent_and_never_an_error() {
    let queue = queue_with(serde_json::json!([
        queue_row(1, serde_json::json!({"title": "Task 1"})),
        queue_row(
            7,
            serde_json::json!({"kind": "stage", "stage_of": 1, "stage_index": 0,
            "stage": {"kind": "deploy", "target": 1, "round": 4}})
        ),
    ]));
    let view = team(&status_with(serde_json::json!([])), &queue);
    let stage = &view
        .groups
        .iter()
        .flat_map(|group| &group.cards)
        .next()
        .unwrap()
        .stages[0];
    assert_eq!(stage.kind, None, "an unknown kind is kept out as absent");
    assert_eq!(stage.target, Some(1), "its target and round still parse");
    assert_eq!(stage.round, Some(4));
}

#[test]
fn the_additive_fields_are_absent_without_an_error_in_an_older_reply() {
    let queue = queue_with(serde_json::json!([queue_row(
        1,
        serde_json::json!({"title": "Task 1"})
    )]));
    let status = status_with(
        serde_json::json!([{"id": "lead", "role": "lead", "agent": "claude", "state": "active"}]),
    );
    let view = team(&status, &queue);
    let card = &view
        .groups
        .iter()
        .find(|group| group.state == "pending")
        .expect("the pending group carries the card")
        .cards[0];
    assert_eq!(card.kind, None);
    assert_eq!(card.rounds, None);
    assert_eq!(view.members[0].model, None);
}

#[test]
fn the_pinned_replies_still_parse_without_the_additive_fields() {
    let view = team(&fixture("status"), &fixture("queue"));
    assert!(view
        .groups
        .iter()
        .flat_map(|group| &group.cards)
        .all(|card| card.kind.is_none() && card.rounds.is_none()));
    assert!(view.members.iter().all(|member| member.model.is_none()));
}

#[test]
fn the_pinned_list_carries_the_lead_session_the_desktop_resolves_with() {
    let view = list(&fixture("list"));
    assert_eq!(view.teams.len(), 1);
    let lead = view.teams[0]
        .lead_session
        .as_ref()
        .expect("the row carries its lead session");
    assert_eq!(lead.machine, "00000000-0000-4000-8000-0000000000aa");
    assert_eq!(
        lead.session_id,
        "launch_id:00000000-0000-4000-8000-000000000064"
    );
}

#[test]
fn a_list_row_without_a_lead_session_or_without_a_session_id_reads_none() {
    for row in [
        serde_json::json!({"id": TEAM, "project": "example-project", "name": "Fixes", "status": "running", "lead": "lead", "home": "aa", "revision": 1}),
        serde_json::json!({"id": TEAM, "project": "example-project", "name": "Fixes", "status": "running", "lead": "lead", "home": "aa", "revision": 1,
            "lead_session": {"machine": "aa", "session_id": ""}}),
    ] {
        let view = list(&serde_json::json!({"ok": true, "rows": [row], "next": null}));
        assert_eq!(view.teams[0].lead_session, None);
    }
}

/// Second review, finding 15: a stage whose target is not on the page keeps
/// its own kind, target and round through the projection; the queue card has
/// no field for them, so its `stages` holds the stage's own facts.
#[test]
fn an_orphan_stage_keeps_its_explicit_pipeline_facts() {
    let queue = queue_with(serde_json::json!([queue_row(
        7,
        serde_json::json!({
            "kind": "stage", "stage_of": 999, "stage_index": 0, "role": "quality", "state": "in_progress",
            "assignee": "reviewer", "implementer": "developer", "stage": {"kind": "review", "target": 999, "round": 2},
        })
    )]));
    let view = team(&status_with(serde_json::json!([])), &queue);
    let own = view
        .groups
        .iter()
        .flat_map(|group| &group.cards)
        .find(|card| card.node == 7)
        .unwrap();
    assert_eq!(own.kind, Some(RowKind::Stage));
    assert_eq!(own.stages.len(), 1);
    let stage = &own.stages[0];
    assert_eq!(
        (stage.node, stage.kind, stage.target, stage.round),
        (7, Some(StageKind::Review), Some(999), Some(2))
    );
    assert_eq!(
        (stage.assignee.as_str(), stage.implementer.as_str()),
        ("reviewer", "developer")
    );
    // A stage folded into its card is not duplicated on that card's own list.
    let folded = queue_with(serde_json::json!([
        queue_row(1, serde_json::json!({"state": "in_progress"})),
        queue_row(
            7,
            serde_json::json!({"kind": "stage", "stage_of": 1, "stage_index": 0, "role": "quality", "state": "in_progress", "stage": {"kind": "review", "target": 1, "round": 1}})
        ),
    ]));
    let view = team(&status_with(serde_json::json!([])), &folded);
    let cards: Vec<_> = view.groups.iter().flat_map(|group| &group.cards).collect();
    assert_eq!(cards.len(), 1);
    assert_eq!(
        cards[0]
            .stages
            .iter()
            .map(|stage| stage.node)
            .collect::<Vec<_>>(),
        vec![7]
    );
}

/// Second confirmation, finding 3 (plan on integrate/teams, "Who is a team's
/// session"): a live team's list row carries `member_sessions`, which the
/// core passes through beside `lead_session`, as the command line's own
/// `crates/team-plan/fixtures/cli-list.json` pins it (the lead among them).
/// An entry without a machine or a session is left out, a row without the
/// field keeps its serialized form, and the entries are bounded.
#[test]
fn a_list_row_passes_its_member_sessions_through() {
    let pinned = list(&fixture("list"));
    let members = pinned.teams[0]
        .member_sessions
        .as_ref()
        .expect("the pinned live row carries its member sessions");
    assert_eq!(
        members,
        &vec![
            MemberSession {
                member: "lead".into(),
                role: "lead".into(),
                machine: "00000000-0000-4000-8000-0000000000aa".into(),
                session_id: "launch_id:00000000-0000-4000-8000-000000000064".into(),
            },
            MemberSession {
                member: "m1".into(),
                role: "developer".into(),
                machine: "00000000-0000-4000-8000-0000000000aa".into(),
                session_id: "launch_id:00000000-0000-4000-8000-000000000031".into(),
            },
        ]
    );
    let serialized = serde_json::to_value(&pinned).unwrap();
    assert_eq!(serialized["teams"][0]["member_sessions"][1]["member"], "m1");
    let mut partial = fixture("list");
    let entries = partial["rows"][0]["member_sessions"]
        .as_array_mut()
        .expect("the pinned row's member sessions are a list");
    entries.push(serde_json::json!({"member": "m2", "machine": "", "session_id": "launch_id:00000000-0000-4000-8000-000000000032"}));
    entries.push(serde_json::json!({"member": "m3"}));
    assert_eq!(
        list(&partial).teams[0].member_sessions.as_ref(),
        Some(members),
        "an entry without a machine or a session is left out"
    );
    let mut without = fixture("list");
    without["rows"][0]
        .as_object_mut()
        .expect("the pinned row is an object")
        .remove("member_sessions");
    let bare = list(&without);
    assert_eq!(bare.teams[0].member_sessions, None);
    assert!(serde_json::to_value(&bare).unwrap()["teams"][0]
        .get("member_sessions")
        .is_none());
    let many: Vec<_> = (0..MEMBERS_KEPT + 10)
        .map(|n| serde_json::json!({"member": format!("m{n}"), "machine": "a", "session_id": format!("launch_id:{n}")}))
        .collect();
    let bounded =
        super::list(&serde_json::json!({"rows": [{"id": TEAM, "member_sessions": many}]}));
    assert_eq!(
        bounded.teams[0].member_sessions.as_ref().unwrap().len(),
        MEMBERS_KEPT
    );
}

/// The store's current replies (`crates/team-plan/fixtures`), which carry the
/// card lists and the row fields of zerus-63rru.29.
fn current(name: &str) -> Value {
    serde_json::from_str(match name {
        "status" => include_str!("../../../../../crates/team-plan/fixtures/cli-status.json"),
        "queue" => include_str!("../../../../../crates/team-plan/fixtures/cli-queue.json"),
        "waiting" => include_str!("../../../../../crates/team-plan/fixtures/cli-waiting.json"),
        "cards" => include_str!("../../../../../crates/team-plan/fixtures/cli-cards.json"),
        other => panic!("no fixture {other}"),
    })
    .unwrap()
}

/// zerus-63rru.29: a card's list, a queue row's holder and pending decision
/// and a member's person_stopped reach the views as the store sends them.
#[test]
fn the_card_lists_and_row_fields_reach_the_views() {
    let mut queue = current("queue");
    queue["rows"][1]["pending_decision"] = json!("accept");
    queue["rows"][1]["holder"] = Value::Null;
    let mut status = current("status");
    status["members"][1]["person_stopped"] = json!(true);
    let view = team(&status, &queue);
    let card = view
        .groups
        .iter()
        .flat_map(|group| &group.cards)
        .find(|card| card.node == 1)
        .unwrap();
    assert_eq!(card.holder, Some(Some("m1".into())));
    assert_eq!(card.pending_decision, None);
    let serialized = serde_json::to_value(card).unwrap();
    assert_eq!(serialized["holder"], "m1");
    assert_eq!(serialized["pending_decision"], Value::Null);
    // The stage row of the pinned queue folds into task 1; on its own page it is a card.
    let stage = team(&status, &json!({"rows": [queue["rows"][1].clone()]}));
    let stage = stage
        .groups
        .iter()
        .flat_map(|group| &group.cards)
        .next()
        .unwrap();
    assert_eq!(stage.holder, Some(None), "null is nobody");
    assert_eq!(serde_json::to_value(stage).unwrap()["holder"], Value::Null);
    assert_eq!(stage.pending_decision.as_deref(), Some("accept"));
    let members: Vec<(String, bool)> = view
        .members
        .iter()
        .map(|member| (member.id.clone(), member.person_stopped))
        .collect();
    assert_eq!(members[1], ("m1".into(), true));
    assert!(members
        .iter()
        .filter(|(id, _)| id != "m1")
        .all(|(_, stopped)| !stopped));
    let waiting = waiting(&current("waiting"));
    assert_eq!(waiting.items[0].decisions, ["acknowledged"]);
    assert_eq!(
        serde_json::to_value(&waiting).unwrap()["items"][0]["decisions"],
        json!(["acknowledged"])
    );
}

/// A row from an older hgs (the copies at fa46dcc08) has none of the fields:
/// the holder is unknown (left out, not "nobody"), no decision is pending,
/// nobody was stopped, and a card takes acknowledged alone.
#[test]
fn an_older_row_reads_without_the_new_fields() {
    let view = team(&fixture("status"), &fixture("queue"));
    for card in view.groups.iter().flat_map(|group| &group.cards) {
        assert_eq!(card.holder, None);
        assert!(serde_json::to_value(card).unwrap().get("holder").is_none());
        assert_eq!(card.pending_decision, None);
    }
    assert!(view.members.iter().all(|member| !member.person_stopped));
    assert_eq!(
        waiting(&fixture("waiting")).items[0].decisions,
        ["acknowledged"]
    );
}

/// The list is read tolerantly: an entry of another shape, an uppercase or
/// overlong token and a repeat are ignored, never an error; at most four are
/// kept in order; a list that is not a list reads as acknowledged alone.
#[test]
fn a_cards_list_is_read_tolerantly() {
    let row =
        |decisions: Value| json!({"team": TEAM, "seq": 1, "kind": "note", "decisions": decisions});
    let read = |decisions: Value| {
        waiting(&json!({"rows": [row(decisions)]})).items[0]
            .decisions
            .clone()
    };
    assert_eq!(
        read(json!([
            7,
            "Retry",
            "retry",
            "x".repeat(33),
            "retry",
            "approve",
            "reject",
            "snooze",
            "acknowledged"
        ])),
        ["retry", "approve", "reject", "snooze"]
    );
    assert_eq!(read(json!("retry")), ["acknowledged"]);
    assert_eq!(read(Value::Null), ["acknowledged"]);
    assert_eq!(read(json!([])), Vec::<String>::new());
}

/// The waiting view of one card of each shape the view must offer by its list
/// alone, pinned as `fixtures/waiting-decisions.json`, which the frontend's test
/// renders: a retry-only card, a blocked launch with acknowledged alone, the
/// store's integration pair, the actor's gate and held cards (retry, then
/// acknowledged, from teams/wp08c-cards), a card with a token this version has
/// no words for, a note, and a row from an older hgs without a list.
#[test]
fn the_waiting_decisions_fixture_is_the_cores_view() {
    let rows = json!([
        {"team": TEAM, "seq": 1, "kind": "launch_capacity", "text": "Team member creation: no_account_available.", "state": "open", "created_ms": 1, "decisions": ["retry"]},
        {"team": TEAM, "seq": 2, "kind": "launch_blocked", "text": "Member m2: worktree_unavailable.", "state": "open", "created_ms": 2, "decisions": ["acknowledged"]},
        {"team": TEAM, "seq": 3, "kind": "integration", "node": 2, "text": "Approve integration of the accepted result.", "state": "open", "created_ms": 3, "decisions": ["approve", "reject"]},
        {"team": TEAM, "seq": 4, "kind": "team_actor_blocked", "text": "gate:integrate:2: The integration branch moved outside the team.", "state": "open", "created_ms": 4, "decisions": ["retry", "acknowledged"]},
        {"team": TEAM, "seq": 5, "kind": "team_actor_blocked", "node": 3, "text": "Task 3: the held decision return waits for its observation.", "state": "open", "created_ms": 5, "decisions": ["retry", "acknowledged"]},
        {"team": TEAM, "seq": 6, "kind": "note", "text": "A card with a decision this version does not know.", "state": "open", "created_ms": 6, "decisions": ["snooze", "acknowledged"]},
        {"team": TEAM, "seq": 7, "kind": "note", "text": "Tell the team the release moved to Monday.", "state": "open", "created_ms": 7, "decisions": ["acknowledged"]},
        {"team": TEAM, "seq": 8, "kind": "report_waiting", "node": 3, "text": "The report on task 3 has waited 45 minutes for the lead's decision.", "state": "open", "created_ms": 8},
    ]);
    let view =
        waiting(&json!({"ok": true, "actor": {"kind": "person"}, "rows": rows, "next": null}));
    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/waiting-decisions.json")).unwrap();
    assert_eq!(serde_json::to_value(&view).unwrap(), fixture);
}

/// The queue rows of zerus-63rru.43's cases: a pending Return that has not
/// started (replaceable), one that has (not replaceable), a pending acceptance
/// (never replaced), a pending reassignment of a held task (replaceable), a row
/// from an older hgs without the flag, and a replaceable decision of a kind the
/// view has no words for.
fn pending_rows() -> Value {
    let base = current("queue")["rows"][0].clone();
    let row = |id: u64, state: &str, held: bool, pending: &str, replaceable: Option<bool>| {
        let mut row = base.clone();
        row["id"] = json!(id);
        row["title"] = json!(format!("Task {id}"));
        row["state"] = json!(state);
        row["hold"] = json!(held);
        row["holder"] = json!("m1");
        row["rank"] = json!(format!("a0{id}"));
        row["task"]["uuid"] = json!(format!("00000000-0000-4000-8000-00000000000{id}"));
        row["pending_decision"] = json!(pending);
        match replaceable {
            Some(flag) => row["pending_replaceable"] = json!(flag),
            None => {
                row.as_object_mut().unwrap().remove("pending_replaceable");
            }
        }
        row
    };
    json!({"ok": true, "actor": {"kind": "person"}, "next": null, "rows": [
        row(1, "reported", false, "return", Some(true)),
        row(2, "reported", false, "return", Some(false)),
        row(3, "reported", false, "accept", Some(false)),
        row(4, "pending", true, "reassign", Some(true)),
        row(5, "reported", false, "return", None),
        row(6, "reported", false, "route", Some(true)),
    ]})
}

/// zerus-63rru.43: the queue card carries pending_replaceable as the store
/// computes it; a row without the field (an older hgs) reads false. The board
/// view of the six cases is pinned as fixtures/board-pending.json, which the
/// frontend's test renders.
#[test]
fn the_pending_decision_rows_are_the_cores_view() {
    let view = team(&current("status"), &pending_rows());
    let flags: Vec<(u64, Option<String>, bool)> = view
        .groups
        .iter()
        .flat_map(|group| &group.cards)
        .map(|card| {
            (
                card.node,
                card.pending_decision.clone(),
                card.pending_replaceable,
            )
        })
        .collect();
    let mut flags = flags;
    flags.sort();
    assert_eq!(
        flags,
        [
            (1, Some("return".into()), true),
            (2, Some("return".into()), false),
            (3, Some("accept".into()), false),
            (4, Some("reassign".into()), true),
            (5, Some("return".into()), false),
            (6, Some("route".into()), true),
        ]
    );
    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/board-pending.json")).unwrap();
    assert_eq!(serde_json::to_value(&view).unwrap(), fixture);
    // Nothing pending: the flag is false.
    let plain = team(&current("status"), &current("queue"));
    assert!(plain
        .groups
        .iter()
        .flat_map(|group| &group.cards)
        .all(|card| !card.pending_replaceable));
}

/// A card row carries the service's bounded parameters (zerus-63rru.34). The
/// row here is a synthetic DTO for the projection's own contract; the service
/// writer bounds the pairs and the CLI sends them as `[[key, value]]`.
#[test]
fn a_card_row_carries_the_services_bounded_parameters() {
    let reply = json!({
        "rows": [{
            "seq": 1,
            "team": TEAM,
            "kind": "launch_blocked",
            "node": null,
            "actor": {"kind": "service", "person": "local"},
            "text": "Member m1: no_account. Try again later.",
            "state": "open",
            "created_ms": 1700000000000i64,
            "decisions": ["acknowledged"],
            "parameters": [["member", "m1"], ["code", "no_account"]]
        }],
        "next": null
    });
    let view = waiting(&reply);
    assert_eq!(
        view.items[0].parameters,
        Some(CardParameters::Pairs(vec![
            ("member".to_owned(), "m1".to_owned()),
            ("code".to_owned(), "no_account".to_owned())
        ]))
    );
    assert_eq!(
        serde_json::to_value(&view).unwrap()["items"][0]["parameters"],
        json!([["member", "m1"], ["code", "no_account"]])
    );
}

/// The five faulted shapes the round-2 review found normalized away before
/// the window, and its controls, through the real read, view, TeamsUpdate,
/// coalescer and serialized/deserialized Batch (zerus-63rru.34, F6).
///
/// The reply rows are deliberately faulted metadata (hand-written), not rows
/// the service's bounded writer produced: the writer's valid pairs are pinned
/// by `a_card_row_carries_the_services_bounded_parameters` and the root launch
/// tests. The projection must keep the invalid state instead of dropping,
/// cutting or replacing it, so the window can show the stored sentence.
#[cfg(unix)]
#[test]
fn invalid_card_parameters_survive_the_waiting_read_as_the_invalid_state() {
    use crate::upstream_teams::read::{read, TeamRead};
    use std::os::unix::fs::PermissionsExt;

    fn row(seq: u64, text: &str, parameters: Value) -> Value {
        json!({
            "seq": seq, "team": TEAM, "kind": "launch_blocked", "node": null,
            "actor": {"kind": "service", "person": "local"},
            "text": text, "state": "open", "created_ms": 1700000000000i64,
            "decisions": ["acknowledged"], "parameters": parameters,
        })
    }
    let nine = |last: Value| {
        let mut pairs: Vec<Value> = (0..8).map(|n| json!([format!("k{n}"), "v"])).collect();
        pairs.push(last);
        Value::Array(pairs)
    };
    let mut rows = vec![
        row(
            100,
            "Stored sentence for triplet",
            json!([["member", "Alice", "extra"]]),
        ),
        row(
            101,
            "Stored sentence for mixed-invalid-pair",
            json!([["member", "Alice"], ["code", false]]),
        ),
        row(
            102,
            "Stored sentence for wrong-top-level-type",
            json!("invalid"),
        ),
        row(103, "Stored sentence for null-metadata", Value::Null),
        row(
            104,
            "Stored sentence for nine-pairs-one-filtered",
            nine(json!(["member", "Alice", "extra"])),
        ),
        row(
            105,
            "Stored sentence for nine-valid-pairs",
            nine(json!(["member", "Alice"])),
        ),
        row(
            106,
            "Stored sentence for bidi-value",
            json!([["member", "a\u{202e}b"]]),
        ),
        row(
            107,
            "Stored sentence for valid-pairs",
            json!([["member", "m1"], ["code", "no_account"]]),
        ),
        row(108, "Stored sentence for empty-pairs", json!([])),
    ];
    // The absent state: the field is not there at all.
    rows.push(row(109, "Stored sentence for absent-metadata", Value::Null));
    rows[9]
        .as_object_mut()
        .unwrap()
        .remove("parameters")
        .unwrap();
    let reply = json!({
        "ok": true,
        "actor": {"kind": "person", "person": "local"},
        "rows": rows,
        "next": null,
    });

    // A tiny hgs stand-in: the real read runs this child and decodes its reply.
    let dir = std::env::temp_dir().join(format!("zerus-wp34r3-{}", uuid::Uuid::new_v4()));
    struct TempDir(std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _tmp = TempDir(dir.clone());
    std::fs::create_dir_all(&dir).unwrap();
    let reply_path = dir.join("reply.json");
    std::fs::write(&reply_path, serde_json::to_vec(&reply).unwrap()).unwrap();
    let hgs = dir.join("hgs");
    std::fs::write(
        &hgs,
        format!("#!/bin/sh\nexec /bin/cat '{}'\n", reply_path.display()),
    )
    .unwrap();
    std::fs::set_permissions(&hgs, std::fs::Permissions::from_mode(0o700)).unwrap();

    let value = read(&hgs, &TeamRead::Waiting { after: None }).expect("the faulted reply reads");
    let view = waiting(&value);
    assert_eq!(view.items.len(), 10);
    let states: Vec<Option<CardParameters>> = view
        .items
        .iter()
        .map(|item| item.parameters.clone())
        .collect();
    assert_eq!(states[0], Some(CardParameters::Invalid));
    assert_eq!(states[1], Some(CardParameters::Invalid));
    assert_eq!(states[2], Some(CardParameters::Invalid));
    assert_eq!(states[3], None, "null is absent");
    assert_eq!(states[4], Some(CardParameters::Invalid));
    assert_eq!(states[5], Some(CardParameters::Invalid));
    assert_eq!(states[6], Some(CardParameters::Invalid));
    assert_eq!(
        states[7],
        Some(CardParameters::Pairs(vec![
            ("member".to_owned(), "m1".to_owned()),
            ("code".to_owned(), "no_account".to_owned()),
        ])),
        "the writer's bounded list stays valid"
    );
    assert_eq!(
        states[8],
        Some(CardParameters::Pairs(Vec::new())),
        "a valid empty list is not absent"
    );
    assert_eq!(states[9], None, "a missing field is absent");

    // The window's only input is this Batch payload: the three states must
    // arrive there as they are.
    let update = crate::TeamsUpdate::Waiting(std::sync::Arc::new(view));
    let mut coalescer = crate::Coalescer::new();
    coalescer.push(crate::Change::teams(
        crate::ComputerId("wp34r3".into()),
        "waiting".into(),
        update,
    ));
    let wire = serde_json::to_value(coalescer.take()).unwrap();
    let decoded: crate::Batch = serde_json::from_value(wire.clone()).expect("the batch decodes");
    assert_eq!(decoded.teams.len(), 1);
    let items = wire["teams"][0]["payload"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 10);
    for item in &items[..3] {
        assert_eq!(item["parameters"], "invalid", "{item}");
    }
    for item in &items[3..4] {
        assert!(item.get("parameters").is_none(), "null is absent: {item}");
    }
    for item in &items[4..7] {
        assert_eq!(item["parameters"], "invalid", "{item}");
    }
    assert_eq!(
        items[7]["parameters"],
        json!([["member", "m1"], ["code", "no_account"]])
    );
    assert_eq!(items[8]["parameters"], json!([]));
    assert!(items[9].get("parameters").is_none(), "{items:?}");
    for (item, row) in items.iter().zip(reply["rows"].as_array().unwrap()) {
        assert_eq!(
            item["text"], row["text"],
            "the original detail stays visible"
        );
    }
}

/// Pending-decision variants are handwritten controls, serialized by the core view writer.
#[test]
#[ignore = "regenerates the board view from the product CLI status and the existing pending-decision cases"]
fn regenerate_pending_board_view() {
    let view = team(&current("status"), &pending_rows());
    let view = serde_json::to_value(&view).unwrap();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/upstream_teams/fixtures/board-pending.json");
    std::fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(&view).unwrap()),
    )
    .unwrap();
}

/// zerus-3u5yu (5): a stage card standing alone (its target is outside the
/// page) names its target's earlier implementers from the row, whom the
/// stage's Reassign refuses; an empty or overlong name is dropped. The rows
/// are the command line's own current product fixture; the earlier
/// implementers and the missing target are hand-written.
#[test]
fn handwritten_a_stage_card_names_its_targets_earlier_implementers() {
    let status: Value = serde_json::from_str(include_str!(
        "../../../../../crates/team-plan/fixtures/cli-status.json"
    ))
    .unwrap();
    let mut queue: Value = serde_json::from_str(include_str!(
        "../../../../../crates/team-plan/fixtures/cli-queue.json"
    ))
    .unwrap();
    let rows = queue["rows"].as_array_mut().unwrap();
    rows.retain(|row| row["kind"] == "stage");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["earlier_implementers"], json!([]));
    rows[0]["earlier_implementers"] = json!(["m1", "", "x".repeat(NAME_CHARS + 1), "m2"]);
    let view = super::board::team(&status, &queue);
    let card = view
        .groups
        .iter()
        .flat_map(|group| &group.cards)
        .find(|card| card.kind == Some(RowKind::Stage))
        .expect("the stage stands alone");
    assert_eq!(card.earlier_implementers, ["m1", "m2"]);
    // A task card names none, and its view omits the field.
    let tasks = super::board::team(&status, &fixture("queue"));
    let task = tasks
        .groups
        .iter()
        .flat_map(|group| &group.cards)
        .next()
        .unwrap();
    assert!(task.earlier_implementers.is_empty());
    assert!(serde_json::to_value(task)
        .unwrap()
        .get("earlier_implementers")
        .is_none());
}

/// zerus-3u5yu (6, mutant rs4 of the #2527 review): Route offers a pipeline
/// only under a role of the team's bundle. The roles and pipelines are the
/// built-in catalog's; the pipeline of an unknown role is hand-written.
#[test]
fn handwritten_routing_offers_only_pipelines_whose_implementer_is_a_bundle_role() {
    let catalog = zerus_team_plan::builtin();
    let mut pipelines = serde_json::to_value(&catalog.pipelines).unwrap();
    let mut ghost = pipelines[0].clone();
    ghost["id"] = json!("ghostly");
    ghost["implementer"] = json!("ghost");
    pipelines.as_array_mut().unwrap().push(ghost);
    let mut status = fixture("status");
    status["team"]["bundle"] = json!({"roles": catalog.roles, "pipelines": pipelines});
    let view = super::board::team(&status, &fixture("queue"));
    assert!(view
        .routing_pipelines
        .iter()
        .any(|choice| choice.id == catalog.pipelines[0].id));
    assert!(view
        .routing_pipelines
        .iter()
        .all(|choice| choice.id != "ghostly" && choice.role != "ghost"));
}
