//! The Teams read model against real `hgs team ... --json` replies of a running
//! team (sanitized: long texts and local paths are synthetic; every key and the
//! shape are as `hgs` 1.56 printed them), in `tests/fixtures/teams`.
use super::model::{team_key, TeamsFollow, TeamsModel, TeamsUpdate, FEED_KEPT, POLL};
use super::read::{decode, invocation, TeamRead, FEED_PAGE, LIST_LIMIT, WAITING_PAGE};
use super::view;
use crate::actions::Exit;
use crate::batch::Coalescer;
use crate::reads::ReadError;
use crate::{Change, ComputerId};
use serde_json::{json, Value};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const TEAM: &str = "33fbafeb-8291-4eda-a00c-6e1448ec6f20";

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/teams/team-{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}")),
    )
    .unwrap()
}

fn exit(value: &Value) -> Exit {
    Exit::Code {
        code: 0,
        stdout: value.to_string().into_bytes(),
        stderr: String::new(),
    }
}

fn status() -> TeamRead {
    TeamRead::Status { team: TEAM.into() }
}

#[test]
fn reads_ask_as_qt_does() {
    let peer = ComputerId::from("studio");
    let cases = [
        (
            TeamRead::List,
            json!({"include_cancelled": true, "limit": LIST_LIMIT}),
            "list",
        ),
        (
            status(),
            json!({"team": TEAM, "include_cancelled": true}),
            "status",
        ),
        (
            TeamRead::Events {
                team: TEAM.into(),
                before: None,
            },
            json!({"team": TEAM, "limit": FEED_PAGE}),
            "events",
        ),
        (
            TeamRead::Events {
                team: TEAM.into(),
                before: Some(13318),
            },
            json!({"team": TEAM, "limit": FEED_PAGE, "before": 13318}),
            "events",
        ),
        (
            TeamRead::Inbox { team: TEAM.into() },
            json!({"team": TEAM}),
            "inbox",
        ),
        (
            TeamRead::Waiting { before: None },
            json!({"limit": WAITING_PAGE}),
            "waiting",
        ),
        (
            TeamRead::Waiting {
                before: Some(json!({"since": 5.0, "id": "a"})),
            },
            json!({"limit": WAITING_PAGE, "before": {"since": 5.0, "id": "a"}}),
            "waiting",
        ),
        (
            TeamRead::Spans {
                team: TEAM.into(),
                after: 4,
            },
            json!({"team": TEAM, "after": 4, "limit": 50}),
            "spans",
        ),
        (
            TeamRead::Profile { team: TEAM.into() },
            json!({"team": TEAM}),
            "profile",
        ),
    ];
    for (read, request, op) in cases {
        let local = invocation(&ComputerId::local(), &read).unwrap();
        assert_eq!(local.args, ["team", op, "--json"], "{read:?}");
        assert_eq!(local.timeout, Duration::from_secs(15));
        let sent: Value = serde_json::from_slice(local.stdin.as_deref().unwrap()).unwrap();
        assert_eq!(sent, request, "{read:?}");
        let remote = invocation(&peer, &read).unwrap();
        assert_eq!(remote.args, ["@studio", "team", op, "--json"]);
        assert_eq!(remote.timeout, Duration::from_secs(45));
    }
    // A request names a real team and a real computer, or runs nothing.
    for team in ["", "--all", "two words", &"x".repeat(65)] {
        assert!(matches!(
            invocation(
                &ComputerId::local(),
                &TeamRead::Status { team: team.into() }
            ),
            Err(ReadError::BadRequest { .. })
        ));
    }
    assert!(matches!(
        invocation(&ComputerId::from("-oProxyCommand=x"), &TeamRead::List),
        Err(ReadError::BadRequest { .. })
    ));
}

#[test]
fn replies_are_checked_for_the_shape_each_view_relies_on() {
    for (read, name) in [
        (TeamRead::List, "list"),
        (status(), "status"),
        (
            TeamRead::Events {
                team: TEAM.into(),
                before: None,
            },
            "events",
        ),
        (TeamRead::Inbox { team: TEAM.into() }, "inbox"),
        (TeamRead::Waiting { before: None }, "waiting"),
        (
            TeamRead::Spans {
                team: TEAM.into(),
                after: 0,
            },
            "spans",
        ),
        (TeamRead::Profile { team: TEAM.into() }, "profile"),
        (TeamRead::Catalog, "catalog"),
    ] {
        let value = fixture(name);
        assert_eq!(decode(&read, exit(&value)).as_ref(), Ok(&value), "{name}");
    }
    let invalid = |read: &TeamRead, reply: &str| {
        matches!(
            decode(
                read,
                Exit::Code {
                    code: 0,
                    stdout: reply.into(),
                    stderr: String::new()
                }
            ),
            Err(ReadError::Invalid { .. })
        )
    };
    assert!(invalid(&TeamRead::List, "[]"));
    assert!(invalid(&TeamRead::List, r#"{"items": {}}"#));
    assert!(invalid(&TeamRead::Catalog, r#"{"roles": []}"#));
    assert!(invalid(&status(), r#"{"id": "t", "members": []}"#));
    assert!(invalid(&status(), "not json"));
    assert_eq!(
        decode(
            &status(),
            Exit::Code {
                code: 1,
                stdout: Vec::new(),
                stderr: "hgs: Unknown team\n".into()
            }
        ),
        Err(ReadError::Failed {
            code: Some(1),
            message: "hgs: Unknown team".into()
        })
    );
}

#[test]
fn the_catalog_reply_is_projected_and_its_read_names_the_catalog() {
    let value = fixture("catalog");
    let catalog = view::catalog(&value);
    assert_eq!((catalog.version, catalog.builtin, catalog.draft_base), (3, false, 2));
    assert_eq!(catalog.roles.len(), 2);
    assert_eq!((catalog.roles[1].id.as_str(), catalog.roles[1].model.as_str()), ("tester", "kimi-k2"));
    assert_eq!(catalog.templates[0].max_members, 6);
    assert_eq!(catalog.templates[0].roster[0].role, "tester");
    assert_eq!(catalog.routing[0].r#use, "feature");
    assert_eq!(catalog.pipelines[0].stages[0].kind, "test");
    assert_eq!(catalog.proposals[0].state, "kept");
    assert_eq!(catalog.proposals[0].value["model"], "Sonnet");
    let request = invocation(&ComputerId::local(), &TeamRead::Catalog).unwrap();
    assert_eq!(request.args, ["team", "catalog", "get", "--json"]);
    assert_eq!(request.stdin.as_deref(), Some(&b"{}"[..]));
}

#[test]
fn the_list_projects_each_team_with_its_members_and_counts() {
    let list = view::list(&fixture("list"));
    assert_eq!(list.teams.len(), 3);
    assert!(!list.truncated);
    let team = &list.teams[0];
    assert_eq!(
        (team.id.as_str(), team.name.as_str(), team.status.as_str()),
        (TEAM, "Zerus Next for the fork", "running")
    );
    // The lead's strip: phase 1 has two accepted of four tasks, one phase total.
    assert_eq!((team.phase, team.phase_done, team.phase_total), (1, 2, 4));
    assert_eq!((team.phases_done, team.phases_total), (0, 1));
    assert_eq!(
        (
            team.members.len(),
            team.lead.as_str(),
            team.tasks,
            team.accepted,
            team.open_proposals
        ),
        (7, "lead", 4, 8, 1)
    );
    let lead = team.members.iter().find(|member| member.lead).unwrap();
    assert_eq!(
        lead.session.as_ref().unwrap().name,
        "claude/Zerus/work-6926cceb"
    );
    assert!(!team.draft());
    assert_eq!(list.teams[1].status, "paused");
    // A list over the cap keeps the newest LIST_KEPT and says so.
    let items: Vec<Value> = (0..view::LIST_KEPT + 7)
        .map(|n| json!({"id": format!("t{n}"), "name": "T"}))
        .collect();
    let long = view::list(&json!({"items": items}));
    assert_eq!((long.teams.len(), long.truncated), (view::LIST_KEPT, true));
}

/// Task #6 (team ede724c4): hgs's list now gives counts and the phase strip
/// instead of tasks, attention items and proposals. The same teams in the old
/// shape (a peer's older hgs) and the summary shape show the same cards and
/// strips.
#[test]
fn the_summary_list_shows_the_same_cards_as_the_old_one() {
    let old = fixture("list");
    let summary = fixture("list-summary");
    for item in summary["items"].as_array().unwrap() {
        for absent in ["tasks", "phases", "attention", "proposals"] {
            assert!(item.get(absent).is_none(), "{absent}");
        }
    }
    assert_eq!(view::list(&summary), view::list(&old));
    let team = &view::list(&summary).teams[0];
    assert_eq!((team.tasks, team.phase, team.phase_done, team.phase_total), (4, 1, 2, 4));
    // An empty strip and no counts read zero, as an empty old list did.
    let bare = view::list(&json!({"items":[{"id":"t","task_count":0,"progress":{},"attention_count":0,"open_proposals":0}]}));
    assert_eq!((bare.teams[0].tasks, bare.teams[0].phase_total, bare.teams[0].attention, bare.teams[0].open_proposals), (0, 0, 0, 0));
}

/// Task #6 (team ede724c4): a real store's list answered by the old hgs and by
/// this one decodes to the same cards and strips. It runs only when
/// HGS_TEST_LIST_OLD and HGS_TEST_LIST_NEW name the two answers.
#[test]
fn a_real_list_decodes_the_same_in_both_shapes() {
    let (Some(old), Some(new)) = (std::env::var_os("HGS_TEST_LIST_OLD"), std::env::var_os("HGS_TEST_LIST_NEW")) else { return };
    let read = |path: &std::ffi::OsStr| -> Value { serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap() };
    let (old, new) = (view::list(&read(&old)), view::list(&read(&new)));
    assert_eq!(old.teams.len(), new.teams.len());
    for (before, after) in old.teams.iter().zip(&new.teams) {
        assert_eq!(before, after, "{}", before.name);
    }
    println!("same cards for {} teams", new.teams.len());
}

#[test]
fn a_status_projects_the_team_tab_and_its_plan_graph() {
    let value = fixture("status");
    let team = view::team(&value);
    assert_eq!(
        (team.id.as_str(), team.status.as_str(), team.lead.as_str()),
        (TEAM, "running", "lead")
    );
    assert_eq!(team.members.len(), 7);
    let lead = &team.members[0];
    assert!(lead.lead && lead.role == "Lead");
    let core = team
        .members
        .iter()
        .find(|member| member.id == "core")
        .unwrap();
    assert_eq!(
        (core.role.as_str(), core.agent.as_str(), core.state.as_str()),
        ("Tauri core (Rust)", "claude", "active")
    );
    assert_eq!(
        core.session.as_ref().unwrap().name,
        "claude/Zerus-team-33fbafeb-core/zerus-next-for-the-fork-core"
    );
    assert_eq!(
        (core.live, core.queue.as_slice(), core.stuck),
        (Some(true), &[6][..], false)
    );
    assert_eq!(team.tasks.len(), 27);
    let task = team.tasks.iter().find(|task| task.id == 6).unwrap();
    assert_eq!(
        (task.status.as_str(), task.assignee.as_str(), task.phase),
        ("in_progress", "core", 1)
    );
    assert_eq!(
        task.deps,
        vec![view::DepView {
            task: 4,
            hard: true
        }]
    );
    assert_eq!(
        task.output,
        ["client/core/src/teams", "client/src-tauri/src"]
    );
    assert_eq!(team.phases.len(), 1);
    assert_eq!(
        (
            team.phases[0].done,
            team.phases[0].total,
            team.phases[0].state.as_str()
        ),
        (8, 27, "running")
    );
    // Only open proposals become cards; each carries the hash an answer names.
    assert_eq!(team.proposals.len(), 1);
    assert_eq!(
        (
            team.proposals[0].id.as_str(),
            team.proposals[0].kind.as_str()
        ),
        ("p4", "add-to-phase")
    );
    assert!(!team.proposals[0].hash.is_empty() && team.proposals[0].change.is_object());
    assert_eq!(team.ready, [2, 19, 23, 26]);
    assert!(team.cycle.is_empty() && team.draft.is_none() && !team.truncated);
    assert_eq!(team.settings["max_members"], 6);
    // The graph: every task in its hgs layer, every dependency an edge.
    let graph = view::graph(&value, &team);
    assert_eq!((graph.nodes.len(), graph.edges.len()), (27, 39));
    let node = graph.nodes.iter().find(|node| node.id == 6).unwrap();
    assert_eq!(
        (node.layer, node.role.as_str(), node.ready),
        (1, "Tauri core (Rust)", false)
    );
    assert!(graph.edges.contains(&view::GraphEdge {
        from: 4,
        to: 6,
        hard: true
    }));
    assert_eq!(
        graph.layers,
        graph.nodes.iter().map(|node| node.layer).max().unwrap() + 1
    );
    assert!(graph
        .nodes
        .iter()
        .filter(|node| node.ready)
        .map(|node| node.id)
        .eq([2, 19, 23, 26]));
    // A running team is no New team form.
    assert!(view::new_team(&value, &team).is_none());
}

#[test]
fn archived_stubs_keep_their_phase_totals_and_frozen_timing_without_attempts() {
    let value = json!({"id":TEAM,"members":[],"archive_totals":{"phases":205,"tasks":1200},
        "tasks":[{"id":7,"phase":2,"title":"Archived work","assignee":"core","status":"accepted",
            "archived":{"started_at":10,"accepted_at":40,"timing":{"worked_s":23}}}],
        "phases":[{"n":2,"archived":{"tasks":8,"accepted":8}}],
        "phase_summary":[{"n":2,"title":"Closed","state":"done","status":"done","done":8,"total":8}]});
    let team = view::team(&value);
    let task = &team.tasks[0];
    assert!(task.archived);
    assert_eq!(task.worked_s, 23.0);
    assert!(task.questions.is_empty() && task.attempt_id.is_empty() && !task.no_report);
    assert!(team.phases[0].archived);
    assert_eq!((team.phases[0].done, team.phases[0].total), (8, 8));
    assert_eq!(team.archived_phases_not_shown, 204);
    assert_eq!(view::graph(&value, &team).nodes[0].id, 7);
}

#[test]
fn one_task_read_validates_its_identity_and_bounds_the_projected_report() {
    let read = TeamRead::Task { team: TEAM.into(), task: 7 };
    let request = invocation(&ComputerId::local(), &read).unwrap();
    assert_eq!(request.args, ["team", "status", "--json"]);
    assert_eq!(read.request(), json!({"team":TEAM,"task":7}));
    let value = json!({"id":7,"title":"Archived work","status":"accepted","attempts":[
        {"report":{"text":"Old report"}}, {"report":{"text":"R".repeat(view::REPORT_CHARS + 10)}}]});
    let decoded = decode(&read, exit(&value)).unwrap();
    let detail = view::task_detail(&decoded);
    assert_eq!(detail.task.id, 7);
    assert_eq!(detail.report.chars().count(), view::REPORT_CHARS);
    assert!(detail.report_truncated);
    assert!(decode(&read, exit(&json!({"id":8}))).is_err());
    assert!(invocation(&ComputerId::local(), &TeamRead::Task { team: TEAM.into(), task: 0 }).is_err());
}

#[test]
fn task_40_status_carries_held_tasks_pipelines_widenings_and_sub_teams() {
    let status = json!({"id":"t1","name":"Checkout","status":"planning","lead":"lead",
        "members":[{"id":"lead","role":"Lead"},{"id":"api","role":"Backend","profile_widening":{"from":"auto","to":"bypass"}}],
        "tasks":[{"id":1,"title":"API","status":"in_review","held":false,"pipeline":"code","subteam":{"team":"s1"},
                  "pipeline_run":{"name":"Code","accept":"lead","round":1,"max_rounds":3,
                      "stages":[{"kind":"review"},{"kind":"test"}],
                      "runs":[{"stage":0,"kind":"review","member":"rev","state":"delivered"}]}},
                 {"id":2,"title":"Docs","status":"pending","held":true}],
        "children":[{"team":"s1","name":"Sub","status":"running","done":0,"total":2}]});
    let view = super::view::team(&status);
    assert_eq!(
        view.widenings,
        vec![json!({"member":"api","role":"Backend","from":"auto","to":"bypass"})]
    );
    assert_eq!(
        view.children,
        vec![json!({"team":"s1","name":"Sub","status":"running"})]
    );
    let api = &view.tasks[0];
    assert!(api.subteam && !api.held && api.pipeline == "code");
    let run = api.pipeline_run.as_ref().unwrap();
    assert_eq!(
        (run.stages.clone(), run.accept.as_str(), run.max_rounds),
        (vec!["review".to_owned(), "test".to_owned()], "lead", 3)
    );
    assert_eq!(
        run.open
            .as_ref()
            .map(|open| (open.stage, open.kind.as_str(), open.member.as_str())),
        Some((0, "review", "rev"))
    );
    assert!(view.tasks[1].held && view.tasks[1].pipeline_run.is_none() && !view.tasks[1].subteam);
    // Task 52: the delegated task names its sub-team for Cancel's question.
    assert_eq!((api.subteam_team.as_str(), view.tasks[1].subteam_team.as_str()), ("s1", ""));
}

/// Task 52: the list names each team's parent, so a tree action's impact
/// walks every level; a team with no parent, or an older hgs, says "".
#[test]
fn task_52_the_list_names_each_teams_parent() {
    let list = view::list(&json!({"items":[
        {"id":"root","name":"Root","status":"running","lead":"lead","members":[],"parent":null},
        {"id":"s1","name":"Sub","status":"running","lead":"lead","members":[],"parent":{"team":"root","task":2}},
        {"id":"s2","name":"Deeper","status":"running","lead":"lead","members":[],"parent":{"team":"s1","task":1}},
        {"id":"old","name":"Older hgs","status":"running","lead":"lead","members":[]}
    ]}));
    let parents: Vec<(&str, &str)> = list.teams.iter().map(|team| (team.id.as_str(), team.parent.as_str())).collect();
    assert_eq!(parents, [("root", ""), ("s1", "root"), ("s2", "s1"), ("old", "")]);
}

#[test]
fn task_55_a_task_carries_its_newest_attempts_open_questions() {
    let status = json!({"id":"t1","name":"Checkout","status":"running","lead":"lead","members":[{"id":"lead"}],
        "tasks":[{"id":15,"title":"Asks","status":"in_progress","assignee":"tester","attempts":[
            {"n":1,"questions":[{"id":"q0","text":"Old","at":1}]},
            {"n":2,"questions":[{"id":"q1","text":"Keep the old field?","at":1720,"answer":"","answered_at":null},
                                {"id":"q2","text":"Answered","at":1730,"answer":"Yes","answered_at":1740}]}]}]});
    let view = view::team(&status);
    assert_eq!(
        view.tasks[0].questions,
        vec![view::QuestionView {
            id: "q1".into(),
            text: "Keep the old field?".into(),
            at: 1720.0
        }]
    );
}

#[test]
fn a_draft_projects_the_new_team_form() {
    let value = fixture("status-draft");
    let team = view::team(&value);
    let form = view::new_team(&value, &team).expect("a draft is a form");
    assert_eq!((form.team.as_str(), form.draft.revision), (TEAM, 9));
    assert_eq!(form.draft.unseen, ["members", "goal"]);
    assert_eq!(form.draft.owner_edited, ["settings"]);
    assert_eq!(form.members.len(), 7);
    assert!(form.members[0].lead);
    assert_eq!(form.addendum, "Synthetic lead_protocol text.");
    assert_eq!(form.validation.errors[0].kind, "members");
    assert!(form.validation.errors[0]
        .text
        .contains("over the team limit of 6"));
    assert_eq!(form.validation.warnings.len(), 1);
    assert_eq!(form.tasks.len(), 27);
}

/// Task #8 (team ede724c4): New team shows the draft's plan mode, the
/// lead's offer and its reason; hgs leaves out the standard mode.
#[test]
fn a_draft_carries_its_plan_mode_and_the_leads_offer() {
    let standard = json!({"id":"t1","name":"Draft","status":"draft","lead":"lead","members":[{"id":"lead"}],
        "plan_mode_offer":"free","plan_mode_reason":"a stream of small requests"});
    let form = view::new_team(&standard, &view::team(&standard)).expect("a draft is a form");
    assert_eq!(
        (form.plan_mode.as_str(), form.plan_mode_offer.as_str(), form.plan_mode_reason.as_str()),
        ("standard", "free", "a stream of small requests")
    );
    let free = json!({"id":"t1","name":"Draft","status":"draft","lead":"lead","members":[{"id":"lead"}],
        "plan_mode":"free","plan_mode_offer":null,"plan_mode_reason":""});
    let form = view::new_team(&free, &view::team(&free)).expect("a draft is a form");
    assert_eq!((form.plan_mode.as_str(), form.plan_mode_offer.as_str()), ("free", ""));
    let odd = json!({"id":"t1","name":"Draft","status":"draft","lead":"lead","plan_mode_offer":"other"});
    assert_eq!(view::new_team(&odd, &view::team(&odd)).unwrap().plan_mode_offer, "");
}

#[test]
fn inbox_waiting_spans_and_profile_project_their_replies() {
    let inbox = view::inbox(TEAM, &fixture("inbox"));
    assert_eq!(inbox.items.len(), 4);
    assert_eq!(inbox.items[0].kind, "build_broker");
    let waiting = view::waiting(&fixture("waiting"));
    assert_eq!(
        (waiting.items.len(), waiting.total, waiting.more),
        (5, 5, false)
    );
    assert_eq!(waiting.items[0].kind, "owner_word_asked");
    assert!(waiting
        .items
        .iter()
        .all(|item| !item.id.is_empty() && !item.kind.is_empty()));
    // Task 304 (hgs #246): each row's state, and the dismissed rows apart from the count.
    let page = view::waiting(&json!({
        "items": [{"id": "t1/team_paused/1", "kind": "team_paused", "team": "t1", "state": "paused:100.0", "title": "Paused"}],
        "total": 1,
        "dismissed": [{"id": "t1/plan_draft/plan", "kind": "plan_draft", "team": "t1", "state": "plan:7", "blocking": true, "blocks": "Orbit", "dismissed_at": 150.0}],
        "dismissed_total": 1
    }));
    assert_eq!(page.items[0].state, "paused:100.0");
    assert_eq!((page.dismissed.len(), page.dismissed_total, page.total), (1, 1, 1));
    assert_eq!(page.dismissed[0].state, "plan:7");
    assert_eq!(page.dismissed[0].detail["blocks"], "Orbit");
    assert!(view::waiting(&fixture("waiting")).dismissed.is_empty());
    let spans = view::spans(TEAM, &fixture("spans"));
    assert_eq!(
        (spans.tasks.len(), spans.next, spans.truncated),
        (27, None, false)
    );
    let profile = view::profile(TEAM, &fixture("profile"));
    assert_eq!((profile.tasks, profile.accepted), (27, 8));
    assert!(!profile.losses.is_empty() && !profile.roles.is_empty());
}

/// Task 82: the profile's task rows carry each task's lateness against the
/// plan and its overrun against the estimate; a row without a baseline has
/// no lateness, an unstarted task no overrun, an unknown schedule says so.
#[test]
fn profile_task_rows_carry_lateness_and_overrun() {
    let value = json!({
        "summary": {"tasks": 3},
        "tasks": {"items": [
            {"task": 3, "lateness": {"planned_start": 100.0, "planned_end": 700.0, "start_late_s": 1500, "inherited_s": 1200,
                "own_s": 300, "end_late_s": null, "from": 2, "actual_start": 1600.0, "actual_end": null},
             "overrun": {"estimate_s": 600, "work_s": 1020, "overrun_s": 420, "ratio": 1.7, "running": true}},
            {"task": 4, "lateness": {"planned_start": 900.0, "start_late_s": 0, "inherited_s": 0, "own_s": 0, "from": null},
             "overrun": {"not_started": true}},
            {"task": 5, "lateness": {"start_late_s": null, "inherited_s": null, "own_s": null, "unknown": "estimate missing"},
             "overrun": {"estimate_s": null, "work_s": 60, "overrun_s": null, "ratio": null}},
            {"task": 6, "lateness": null, "overrun": {"not_started": true}}
        ], "next_cursor": 6}
    });
    let profile = view::profile(TEAM, &value);
    assert!(profile.task_rows_more);
    let row = |id: u64| profile.task_rows.iter().find(|row| row.task == id).unwrap();
    let three = row(3).lateness.clone().unwrap();
    assert_eq!(
        (
            three.known,
            three.own_s,
            three.inherited_s,
            three.from,
            three.actual_start
        ),
        (true, 300, 1200, Some(2), Some(1600.0))
    );
    let over = row(3).overrun.clone().unwrap();
    assert_eq!(
        (
            over.started,
            over.known,
            over.overrun_s,
            over.ratio,
            over.running
        ),
        (true, true, 420, 1.7, true)
    );
    let four = row(4).lateness.clone().unwrap();
    assert_eq!((four.known, four.start_late_s, four.from), (true, 0, None));
    assert!(!row(4).overrun.clone().unwrap().started);
    let five = row(5).lateness.clone().unwrap();
    assert_eq!(
        (five.known, five.unknown.as_str()),
        (false, "estimate missing")
    );
    assert!(!row(5).overrun.clone().unwrap().known);
    assert!(row(6).lateness.is_none());
}

#[test]
fn waiting_pages_append_new_items_in_order_and_stop_at_the_last() {
    let start = Instant::now();
    let mut model = TeamsModel::default();
    model.follow(
        TeamsFollow {
            waiting: true,
            ..TeamsFollow::default()
        },
        start,
    );
    let first = json!({
        "items": [
            {"id": "a", "kind": "report", "team": TEAM, "title": "A", "since": 3.0},
            {"id": "b", "kind": "report", "team": TEAM, "title": "B", "since": 2.0}
        ],
        "total": 3,
        "next": {"since": 2.0, "id": "b"}
    });
    let out = poll_once(&mut model, start, |_| first.clone());
    assert_eq!(out.iter().filter(|(key, _)| key == "waiting").count(), 1);
    assert!(model.request_waiting_older());
    assert!(!model.request_waiting_older(), "one older page at a time");
    let second = json!({
        "items": [
            {"id": "b", "kind": "report", "team": TEAM, "title": "B", "since": 2.0},
            {"id": "c", "kind": "report", "team": TEAM, "title": "C", "since": 1.0}
        ],
        "total": 3,
        "next": null
    });
    let out = poll_once(&mut model, start + POLL, |_| second.clone());
    let waiting = out
        .iter()
        .find(|(key, _)| key == "waiting")
        .map(|(_, update)| update.payload())
        .expect("the older page sends the combined list");
    assert_eq!(
        waiting["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    assert_eq!(waiting["total"], 3);
    assert!(!model.request_waiting_older(), "the last page has no cursor");
}

fn follow_team() -> TeamsFollow {
    TeamsFollow {
        list: true,
        waiting: true,
        team: Some(TEAM.into()),
        catalog: false,
    }
}

/// Runs every due read against the fixtures and applies it.
fn poll_once(
    model: &mut TeamsModel,
    now: Instant,
    reply: impl Fn(&TeamRead) -> Value,
) -> Vec<(String, TeamsUpdate)> {
    let mut out = Vec::new();
    while let Some(read) = model.next(now) {
        out.extend(model.apply(&read, Ok(reply(&read)), now));
    }
    out
}

fn replies(read: &TeamRead) -> Value {
    match read {
        TeamRead::List => fixture("list"),
        TeamRead::Status { .. } => fixture("status"),
        TeamRead::Task { task, .. } => json!({"id":task}),
        TeamRead::Events { before: None, .. } => fixture("events"),
        TeamRead::Events {
            before: Some(_), ..
        } => fixture("events-older"),
        TeamRead::Inbox { .. } => fixture("inbox"),
        TeamRead::Waiting { .. } => fixture("waiting"),
        TeamRead::Spans { .. } | TeamRead::ScopedSpans { .. } => fixture("spans"),
        TeamRead::Profile { .. } | TeamRead::ScopedProfile { .. } => fixture("profile"),
        TeamRead::Catalog => fixture("catalog"),
        TeamRead::CatalogRoute { .. } => serde_json::json!({"rule": null, "template": null}),
        TeamRead::TestQueue { .. } | TeamRead::TestResults { .. } | TeamRead::TestWindow { .. } => {
            json!({"items":[]})
        }
        TeamRead::Builds { .. } => json!({"folders":[]}),
    }
}

#[test]
fn followed_reads_run_every_three_seconds_and_unchanged_replies_send_nothing() {
    let start = Instant::now();
    let mut model = TeamsModel::default();
    assert!(
        model.next(start).is_none(),
        "nothing followed, nothing read"
    );
    assert_eq!(model.wait(start), POLL);
    assert!(model.follow(follow_team(), start).is_empty());
    let first = poll_once(&mut model, start, replies);
    let keys: Vec<&str> = first.iter().map(|(key, _)| key.as_str()).collect();
    for key in [
        "list",
        "waiting",
        &team_key("team", TEAM),
        &team_key("graph", TEAM),
        &team_key("inbox", TEAM),
        &team_key("feed", TEAM),
    ] {
        assert!(keys.contains(&key), "{key} missing from {keys:?}");
    }
    assert!(
        !keys.contains(&team_key("new", TEAM).as_str()),
        "a running team has no form"
    );
    // Each read ran once; nothing is due before three seconds pass.
    assert!(model.next(start + Duration::from_millis(2900)).is_none());
    assert!(model.wait(start + Duration::from_secs(1)) <= Duration::from_secs(2));
    // The same replies again change no view.
    assert!(poll_once(&mut model, start + POLL, replies).is_empty());
    // A changed team sends its views again; an unchanged list does not.
    let later = poll_once(&mut model, start + POLL * 2, |read| match read {
        TeamRead::Status { .. } => {
            let mut value = fixture("status");
            value["tasks"][0]["status"] = json!("accepted");
            value
        }
        other => replies(other),
    });
    let keys: Vec<&str> = later.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(keys, [team_key("team", TEAM), team_key("graph", TEAM)]);
    // A failed read says so once; the view keeps what it showed.
    let mut failing = TeamsModel::default();
    failing.follow(follow_team(), start);
    let read = failing.next(start).unwrap();
    let error = || {
        Err(ReadError::Failed {
            code: Some(1),
            message: "hgs: Unknown team".into(),
        })
    };
    let out = failing.apply(&read, error(), start);
    assert!(
        matches!(&out[..], [(_, TeamsUpdate::Failed { message, .. })] if message == "hgs: Unknown team")
    );
    // Every followed read fails once and says so...
    while let Some(read) = failing.next(start) {
        assert_eq!(failing.apply(&read, error(), start).len(), 1, "{read:?}");
    }
    // ...and the same failures again send nothing.
    let mut again = 0;
    while let Some(read) = failing.next(start + POLL) {
        again += 1;
        assert!(
            failing.apply(&read, error(), start + POLL).is_empty(),
            "the same failure is not sent twice: {read:?}"
        );
    }
    assert_eq!(again, 5);
}

/// Task 8: the reads a moved revision makes due, in order.
/// A head whose row is the same but for its revision.
fn head(revision: i64) -> super::Head {
    super::Head {
        revision,
        digest: 0,
    }
}

fn due_reads(model: &mut TeamsModel, now: Instant) -> Vec<String> {
    let mut names = Vec::new();
    while let Some(read) = model.next(now) {
        names.push(match &read {
            TeamRead::List => "list".to_owned(),
            TeamRead::Waiting { .. } => "waiting".to_owned(),
            TeamRead::Status { .. } => "status".to_owned(),
            TeamRead::Inbox { .. } => "inbox".to_owned(),
            TeamRead::Events { .. } => "feed".to_owned(),
            other => format!("{other:?}"),
        });
        model.apply(&read, Ok(replies(&read)), now);
    }
    names.sort();
    names
}

#[test]
fn a_hidden_window_reads_only_waiting_at_the_watchers_pace() {
    // Task #10: hidden or minimized, nothing but Waiting is read, and that at
    // the always-on watcher's pace; shown again, every followed view at once.
    let start = Instant::now();
    let mut model = TeamsModel::default();
    model.follow(follow_team(), start);
    assert_eq!(due_reads(&mut model, start).len(), 5);
    model.set_hidden(true, start);
    assert!(model.hidden());
    let mut read = Vec::new();
    let mut at = start;
    while at <= start + super::model::HIDDEN_WAITING * 3 {
        read.extend(due_reads(&mut model, at));
        // Nothing else is due, so the thread waits instead of spinning.
        assert!(model.wait(at) > Duration::ZERO, "{at:?}");
        at += POLL;
    }
    assert_eq!(
        read,
        ["waiting", "waiting", "waiting"],
        "one Waiting read per {:?}",
        super::model::HIDDEN_WAITING
    );
    // An action's refresh does not read the hidden views either.
    model.refresh(at);
    assert!(due_reads(&mut model, at)
        .iter()
        .all(|key| *key == "waiting"));
    // Shown again: every followed view is read now.
    model.set_hidden(false, at);
    assert_eq!(
        due_reads(&mut model, at),
        ["feed", "inbox", "list", "status", "waiting"]
    );
    assert!(
        due_reads(&mut model, at + POLL).len() == 5,
        "polling as before"
    );
}

#[test]
fn hiding_never_starts_a_second_waiting_read() {
    let start = Instant::now();
    let mut model = TeamsModel::default();
    model.follow(
        TeamsFollow {
            waiting: true,
            ..TeamsFollow::default()
        },
        start,
    );
    let running = model.next(start).unwrap();
    assert_eq!(running, TeamRead::Waiting { before: None });
    model.set_hidden(true, start);
    assert_eq!(
        model.next(start + super::model::HIDDEN_WAITING),
        None,
        "the read in flight keeps its slot"
    );
    model.apply(&running, Ok(replies(&running)), start + POLL);
    assert_eq!(model.next(start + POLL * 2), None);
    assert_eq!(
        model.next(start + POLL + super::model::HIDDEN_WAITING),
        Some(TeamRead::Waiting { before: None })
    );
}

#[test]
fn with_heads_the_inbox_and_feed_are_read_within_a_minute_without_a_revision_move() {
    // Task #10, attempt 2: a message to the lead or a feed event is written
    // without a revision bump, so with the stream's heads the watched team's
    // inbox and feed keep the minute-long safety read; a moved revision
    // still makes them due at once.
    use std::sync::Arc;
    let heads = |rev: i64| {
        Some(Arc::new(
            [(TEAM.to_owned(), head(rev)), ("other".to_owned(), head(1))]
                .into_iter()
                .collect::<super::Heads>(),
        ))
    };
    let start = Instant::now();
    let mut model = TeamsModel::default();
    model.follow(follow_team(), start);
    model.heads(heads(1), start);
    assert_eq!(due_reads(&mut model, start).len(), 5);
    assert!(
        due_reads(&mut model, start + POLL * 3).is_empty(),
        "no polling every 3 s"
    );
    // No revision move: the inbox and the feed are read again within STREAM_POLL.
    let reads = due_reads(&mut model, start + super::STREAM_POLL);
    assert!(
        reads.contains(&"inbox".to_owned()) && reads.contains(&"feed".to_owned()),
        "{reads:?}"
    );
    // The watched team's revision moved: at once, not a minute later.
    let at = start + super::STREAM_POLL + POLL;
    model.heads(heads(2), at);
    assert_eq!(
        due_reads(&mut model, at),
        ["feed", "inbox", "list", "status", "waiting"]
    );
}

#[test]
fn a_hidden_poller_reads_only_waiting_and_shares_it_with_the_watcher() {
    // Task #10, with the poller's runner fixture: hidden, the poller runs no
    // list, status, inbox or feed read, still reads Waiting, sends its view
    // and keeps the reply for the always-on watcher, which then reads none.
    use std::sync::{Arc, Mutex};
    let reads: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = Arc::clone(&reads);
    let (tx, rx) = mpsc::channel();
    let poller = super::TeamsPoller::spawn_with(
        ComputerId::local(),
        Box::new(move |read: &TeamRead| {
            log.lock().unwrap().push(format!("{read:?}"));
            Ok(replies(read))
        }),
        Box::new(move |change| {
            let _ = tx.send(change);
        }),
    )
    .unwrap();
    let share = super::WaitingShare::default();
    poller.share_waiting(Some(share.clone()));
    poller.hidden(true);
    poller.follow(follow_team());
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut waiting = false;
    while !waiting && Instant::now() < deadline {
        if let Ok(Change::Teams { key, .. }) = rx.recv_timeout(Duration::from_millis(200)) {
            waiting |= key == "waiting";
        }
    }
    assert!(waiting, "the Waiting view arrives while hidden");
    // Give a busy poller the time to read anything else it would.
    while rx.recv_timeout(Duration::from_millis(500)).is_ok() {}
    let seen = reads.lock().unwrap().clone();
    assert!(
        seen.iter().all(|read| read.starts_with("Waiting")),
        "{seen:?}"
    );
    assert_eq!(seen.len(), 1, "{seen:?}");
    // The watcher reuses that reply instead of starting a read of its own.
    let reply = share
        .reply_or(super::model::HIDDEN_WAITING, || {
            panic!("the watcher read again")
        })
        .unwrap();
    assert_eq!(reply, replies(&TeamRead::Waiting { before: None }));
    // Shown again: the team's views are read.
    poller.hidden(false);
    let deadline = Instant::now() + Duration::from_secs(10);
    let shown_all = |reads: &[String]| {
        ["List", "Status", "Inbox", "Events"]
            .iter()
            .all(|kind| reads.iter().any(|read| read.starts_with(kind)))
    };
    while Instant::now() < deadline && !shown_all(&reads.lock().unwrap()) {
        let _ = rx.recv_timeout(Duration::from_millis(100));
    }
    let seen = reads.lock().unwrap().clone();
    for kind in ["List", "Status", "Inbox", "Events"] {
        assert!(
            seen.iter().any(|read| read.starts_with(kind)),
            "{kind} after showing: {seen:?}"
        );
    }
}

#[test]
fn the_watcher_reads_waiting_once_per_interval_while_a_window_follows_it() {
    // Task #10: a fresh reply from the poller serves the watcher; a stale or
    // missing one is read once and kept.
    let share = super::WaitingShare::default();
    let mut read = 0;
    let reply = json!({"items": [], "total": 0});
    assert_eq!(
        share
            .reply_or(Duration::from_secs(30), || {
                read += 1;
                Ok(reply.clone())
            })
            .unwrap(),
        reply
    );
    assert_eq!(read, 1, "nothing kept yet: one read");
    for _ in 0..3 {
        share
            .reply_or(Duration::from_secs(30), || {
                read += 1;
                Ok(reply.clone())
            })
            .unwrap();
    }
    assert_eq!(read, 1, "within the interval the kept reply serves");
    let old = Instant::now() - Duration::from_secs(31);
    share.put(&reply, old);
    assert!(share
        .fresh(Duration::from_secs(30), Instant::now())
        .is_none());
    share
        .reply_or(Duration::from_secs(30), || {
            read += 1;
            Ok(reply.clone())
        })
        .unwrap();
    assert_eq!(read, 2, "a stale reply is read again");
    // A failed read keeps nothing new and is retried next time.
    let empty = super::WaitingShare::default();
    assert!(empty
        .reply_or(Duration::from_secs(30), || Err(ReadError::TimedOut))
        .is_err());
    assert!(empty
        .fresh(Duration::from_secs(30), Instant::now())
        .is_none());
}

#[test]
fn with_the_streams_team_heads_a_team_is_read_when_its_revision_moved() {
    use std::sync::Arc;
    let heads = |pairs: &[(&str, i64)]| {
        Some(Arc::new(
            pairs
                .iter()
                .map(|(id, rev)| ((*id).to_owned(), head(*rev)))
                .collect::<super::Heads>(),
        ))
    };
    let start = Instant::now();
    let mut model = TeamsModel::default();
    model.follow(follow_team(), start);
    assert_eq!(
        due_reads(&mut model, start),
        ["feed", "inbox", "list", "status", "waiting"]
    );
    // The stream's heads arrive: everything is read once more, then nothing
    // is due after three seconds; a safety read comes after a minute.
    model.heads(heads(&[(TEAM, 5), ("other", 1)]), start);
    assert!(model.streamed());
    assert_eq!(due_reads(&mut model, start).len(), 5);
    assert!(
        due_reads(&mut model, start + POLL * 3).is_empty(),
        "no polling while the stream gives heads"
    );
    // Another team changed: only the list and Waiting are read.
    model.heads(heads(&[(TEAM, 5), ("other", 2)]), start + POLL * 4);
    assert_eq!(due_reads(&mut model, start + POLL * 4), ["list", "waiting"]);
    // The watched team changed: its reads and the list.
    model.heads(heads(&[(TEAM, 6), ("other", 2)]), start + POLL * 5);
    assert_eq!(
        due_reads(&mut model, start + POLL * 5),
        ["feed", "inbox", "list", "status", "waiting"]
    );
    // A change while a read runs reads again at its reply, not a minute later.
    let at = start + POLL * 6;
    model.heads(heads(&[(TEAM, 7), ("other", 2)]), at);
    let running = model.next(at).unwrap();
    // Task #14: the watched team's status runs first.
    assert_eq!(running, TeamRead::Status { team: TEAM.into() });
    model.heads(heads(&[(TEAM, 8), ("other", 2)]), at);
    model.apply(&running, Ok(replies(&running)), at);
    assert_eq!(
        due_reads(&mut model, at),
        ["feed", "inbox", "list", "status", "waiting"],
        "the status again, at once"
    );
    assert_eq!(model.next(at + POLL), None);
    // The safety read after a minute, the inbox and feed included: their
    // messages and events are written without a revision bump.
    assert_eq!(
        due_reads(&mut model, at + super::STREAM_POLL + POLL).len(),
        5
    );
    // The stream lost its heads (a gap, an older peer): polling again.
    model.heads(None, at + super::STREAM_POLL * 2);
    assert!(!model.streamed());
    assert_eq!(due_reads(&mut model, at + super::STREAM_POLL * 2).len(), 5);
    assert_eq!(
        due_reads(&mut model, at + super::STREAM_POLL * 2 + POLL).len(),
        5,
        "every three seconds"
    );
}

/// Task #6 of team c536f0e5 (P1.4, owner 2026-11-23): a peer's team whose
/// revision moves every second with nothing the list shows (its worker's
/// observations) makes the list and Waiting due at most every
/// [`super::REVISION_ONLY`] after their reply, not on every head; a head
/// whose row changed (a status) makes them due at once; the watched team's
/// own reads stay immediate.
#[test]
fn revision_only_heads_read_the_list_at_most_every_three_seconds() {
    use std::sync::Arc;
    let heads = |watched: i64, other: i64, digest: u64| {
        Some(Arc::new(
            [
                (TEAM.to_owned(), head(watched)),
                (
                    "other".to_owned(),
                    super::Head {
                        revision: other,
                        digest,
                    },
                ),
            ]
            .into_iter()
            .collect::<super::Heads>(),
        ))
    };
    let start = Instant::now();
    let mut model = TeamsModel::default();
    model.follow(follow_team(), start);
    model.heads(heads(1, 1, 0), start);
    assert_eq!(due_reads(&mut model, start).len(), 5);
    // A minute of revision-only heads, one a second, the reads answered at once
    // (the watched team's minute-long safety read is due at 60 s).
    let mut lists = 0;
    for second in 1..60u64 {
        let at = start + Duration::from_secs(second);
        model.heads(heads(1, 1 + second as i64, 0), at);
        let reads = due_reads(&mut model, at);
        assert!(!reads.contains(&"status".to_owned()), "{reads:?}");
        lists += reads.iter().filter(|read| *read == "list").count();
    }
    assert_eq!(lists, 19, "once every three seconds instead of 59 times");
    assert_eq!(
        due_reads(&mut model, start + Duration::from_secs(60)).len(),
        5,
        "the safety read"
    );
    // Just after a reply a revision-only head waits; it is read within 3 s.
    let at = start + Duration::from_millis(60_500);
    model.heads(heads(1, 100, 0), at);
    assert!(due_reads(&mut model, at).is_empty());
    assert_eq!(
        due_reads(&mut model, start + Duration::from_secs(63)),
        ["list", "waiting"]
    );
    // A change the list shows: at once, right after a reply.
    let at = start + Duration::from_secs(63) + Duration::from_millis(100);
    model.heads(heads(1, 101, 7), at);
    assert_eq!(due_reads(&mut model, at), ["list", "waiting"]);
    // The watched team's revision moved alone: its reads at once, the list within 3 s.
    let at = at + Duration::from_millis(100);
    model.heads(heads(2, 101, 7), at);
    assert_eq!(due_reads(&mut model, at), ["feed", "inbox", "status"]);
    assert_eq!(
        due_reads(&mut model, at + super::REVISION_ONLY),
        ["list", "waiting"]
    );
    // A revision-only head while the list's read runs: read again within 3 s
    // of the head, not a minute after the reply.
    let at = at + super::REVISION_ONLY * 2;
    model.heads(heads(2, 102, 7), at);
    let mut running = None;
    while let Some(read) = model.next(at) {
        if read == TeamRead::List {
            running = Some(read);
        } else {
            model.apply(&read, Ok(replies(&read)), at);
        }
    }
    let running = running.expect("the list is read");
    model.heads(heads(2, 103, 7), at + Duration::from_secs(1));
    model.apply(&running, Ok(replies(&running)), at + Duration::from_secs(2));
    assert!(due_reads(&mut model, at + Duration::from_secs(2)).is_empty());
    assert_eq!(
        due_reads(&mut model, at + Duration::from_secs(4)),
        ["list", "waiting"]
    );
}

#[test]
fn a_new_follow_ends_the_views_it_no_longer_shows_and_drops_late_replies() {
    let start = Instant::now();
    let mut model = TeamsModel::default();
    model.follow(follow_team(), start);
    poll_once(&mut model, start, replies);
    let in_flight = model.next(start + POLL).unwrap();
    // The tab moves to another team: the old team's views end, once each.
    let ended = model.follow(
        TeamsFollow {
            list: true,
            waiting: false,
            team: Some("other".into()),
            catalog: false,
        },
        start + POLL,
    );
    let mut keys: Vec<String> = ended
        .iter()
        .map(|(key, update)| {
            assert_eq!(update, &TeamsUpdate::Ended);
            key.clone()
        })
        .collect();
    keys.sort();
    let mut expected = vec!["waiting".to_owned()];
    expected.extend(["feed", "graph", "inbox", "team"].map(|kind| team_key(kind, TEAM)));
    expected.sort();
    assert_eq!(keys, expected);
    // A reply about the team no longer watched gives nothing.
    if in_flight.team().is_some() {
        assert!(model
            .apply(&in_flight, Ok(replies(&in_flight)), start + POLL)
            .is_empty());
    }
    // The new team's reads are due at once.
    let next = model.next(start + POLL).unwrap();
    assert!(
        next.team().is_none() || next.team() == Some("other"),
        "{next:?}"
    );
}

/// A long journal: Load older adds pages until FEED_KEPT and then stops; the
/// feed never holds more.
#[test]
fn a_long_journal_stays_within_the_feed_cap() {
    let start = Instant::now();
    let mut model = TeamsModel::default();
    model.follow(
        TeamsFollow {
            team: Some(TEAM.into()),
            ..TeamsFollow::default()
        },
        start,
    );
    let page = |newest: i64| {
        let items: Vec<Value> = (0..FEED_PAGE as i64)
            .map(
                |n| json!({"seq": newest - n, "at": 1.0, "kind": "started", "detail": {"task": n}}),
            )
            .collect();
        json!({"items": items, "next_cursor": newest - FEED_PAGE as i64 + 1})
    };
    let mut newest = 1_000_000;
    poll_once(&mut model, start, |read| match read {
        TeamRead::Events { .. } => page(newest),
        other => replies(other),
    });
    assert_eq!(model.feed_len(), FEED_PAGE as usize);
    let mut cursor = newest - FEED_PAGE as i64 + 1;
    let mut asked = 0;
    while model.request_older() {
        asked += 1;
        let read = model.next(start).unwrap();
        assert_eq!(
            read,
            TeamRead::Events {
                team: TEAM.into(),
                before: Some(cursor)
            }
        );
        let reply = page(cursor - 1);
        cursor -= FEED_PAGE as i64;
        let out = model.apply(&read, Ok(reply), start);
        let TeamsUpdate::Feed(feed) = &out.last().unwrap().1 else {
            panic!("{out:?}")
        };
        assert!(feed.events.len() <= FEED_KEPT);
        assert!(
            feed.events.windows(2).all(|pair| pair[0].seq > pair[1].seq),
            "newest first"
        );
        assert!(asked < 100, "Load older must stop");
    }
    assert_eq!(model.feed_len(), FEED_KEPT);
    // New events push the oldest out; the feed stays at its cap and says so.
    newest += 20;
    let out = poll_once(&mut model, start + POLL, |read| match read {
        TeamRead::Events { .. } => page(newest),
        other => replies(other),
    });
    let feed = out
        .iter()
        .find_map(|(_, update)| match update {
            TeamsUpdate::Feed(feed) => Some(feed.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        (feed.events.len(), feed.capped, feed.more),
        (FEED_KEPT, true, false)
    );
    assert_eq!(feed.events[0].seq, newest);
    assert!(!model.request_older());
    // More than a page since the last read: the feed starts over from the
    // newest page instead of holding a hole.
    newest += 10_000;
    poll_once(&mut model, start + POLL * 2, |read| match read {
        TeamRead::Events { .. } => page(newest),
        other => replies(other),
    });
    assert_eq!(model.feed_len(), FEED_PAGE as usize);
    assert!(model.request_older());
}

#[test]
fn the_real_feed_pages_join_and_load_older_names_the_cursor() {
    let start = Instant::now();
    let mut model = TeamsModel::default();
    model.follow(
        TeamsFollow {
            team: Some(TEAM.into()),
            ..TeamsFollow::default()
        },
        start,
    );
    poll_once(&mut model, start, replies);
    assert_eq!(model.feed_len(), 50);
    assert!(model.request_older());
    assert!(!model.request_older(), "one older page at a time");
    let read = model.next(start).unwrap();
    assert_eq!(
        read,
        TeamRead::Events {
            team: TEAM.into(),
            before: Some(13318)
        }
    );
    let out = model.apply(&read, Ok(fixture("events-older")), start);
    let TeamsUpdate::Feed(feed) = &out[0].1 else {
        panic!("{out:?}")
    };
    assert_eq!(
        (
            feed.events.len(),
            feed.events[0].seq,
            feed.events[99].seq,
            feed.more
        ),
        (100, 13367, 13268, true)
    );
    assert_eq!(feed.events[0].kind, "started");
    assert!(feed.events[0].detail.is_object());
}

#[test]
fn spans_and_profile_are_read_on_request_for_the_watched_team_only() {
    let start = Instant::now();
    let mut model = TeamsModel::default();
    assert!(
        !model.request_spans(0) && !model.request_profile(),
        "no team watched"
    );
    model.follow(
        TeamsFollow {
            team: Some(TEAM.into()),
            ..TeamsFollow::default()
        },
        start,
    );
    poll_once(&mut model, start, replies);
    assert!(model.request_spans(0) && model.request_profile());
    let out = poll_once(&mut model, start, replies);
    let keys: Vec<&str> = out.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(keys, [team_key("spans", TEAM), team_key("profile", TEAM)]);
}

/// Task 72 (F3): Waiting for you is read as soon as it is followed, not after
/// a poll interval, and an empty reply is an empty view ("Nothing waits for
/// you"), never the reading state.
#[test]
fn waiting_is_read_at_once_and_an_empty_list_is_a_view() {
    let (tx, rx) = mpsc::channel();
    let poller = super::TeamsPoller::spawn_with(
        ComputerId::local(),
        Box::new(|read: &TeamRead| match read {
            TeamRead::Waiting { .. } => Ok(json!({"items": [], "total": 0, "next": null})),
            other => Ok(replies(other)),
        }),
        Box::new(move |change| {
            let _ = tx.send(change);
        }),
    )
    .unwrap();
    let asked = Instant::now();
    poller.follow(TeamsFollow {
        waiting: true,
        ..TeamsFollow::default()
    });
    let change = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the first Waiting view");
    assert!(
        asked.elapsed() < super::model::POLL,
        "read after {:?}, not at once",
        asked.elapsed()
    );
    let mut coalescer = Coalescer::new();
    coalescer.push(change);
    let batch = coalescer.take();
    let waiting = batch
        .teams
        .iter()
        .find(|out| out.key == "waiting")
        .expect("a waiting entry");
    assert_eq!(waiting.kind, "waiting");
    assert_eq!(waiting.payload["items"], json!([]));
    assert_eq!(waiting.payload["total"], 0);
}

/// The poller runs the reads off the calling thread and the coalescer folds
/// its changes to one entry per key, each carrying a projected view.
#[test]
fn the_poller_feeds_one_batch_entry_per_view() {
    let (tx, rx) = mpsc::channel();
    let poller = super::TeamsPoller::spawn_with(
        ComputerId::local(),
        Box::new(|read: &TeamRead| Ok(replies(read))),
        Box::new(move |change| {
            let _ = tx.send(change);
        }),
    )
    .unwrap();
    poller.follow(follow_team());
    let mut coalescer = Coalescer::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut keys = std::collections::BTreeSet::new();
    while keys.len() < 6 && Instant::now() < deadline {
        if let Ok(change) = rx.recv_timeout(Duration::from_millis(200)) {
            if let Change::Teams { key, .. } = &change {
                keys.insert(key.clone());
            }
            coalescer.push(change);
        }
    }
    assert_eq!(keys.len(), 6, "{keys:?}");
    let batch = coalescer.take();
    assert_eq!(batch.teams.len(), 6);
    let team = batch
        .teams
        .iter()
        .find(|out| out.key == team_key("team", TEAM))
        .unwrap();
    assert_eq!(team.kind, "team");
    // The payload is the view model, not the hgs reply.
    assert_eq!(team.payload["members"][1]["id"], "core");
    assert!(team.payload.get("health").is_some() && team.payload.get("phase_summary").is_none());
    let graph = batch.teams.iter().find(|out| out.kind == "graph").unwrap();
    assert_eq!(graph.payload["nodes"].as_array().unwrap().len(), 27);
    // Leaving the team ends its views through the same path.
    poller.follow(TeamsFollow::default());
    let mut ended = 0;
    let deadline = Instant::now() + Duration::from_secs(10);
    while ended < 6 && Instant::now() < deadline {
        if let Ok(Change::Teams {
            update: TeamsUpdate::Ended,
            ..
        }) = rx.recv_timeout(Duration::from_millis(200))
        {
            ended += 1;
        }
    }
    assert_eq!(ended, 6);
    drop(poller);
}

/// A failure never replaces a view the batch has not sent yet.
#[test]
fn each_update_carries_its_payload_once_into_the_batch() {
    // Task #9: the payload the digest hashed is the batch's payload, and the
    // same reply again sends nothing.
    let start = Instant::now();
    let mut model = TeamsModel::default();
    model.follow(follow_team(), start);
    let mut coalescer = Coalescer::new();
    let mut sent_keys = Vec::new();
    while let Some(read) = model.next(start) {
        for sent in model.apply_sent(&read, Ok(replies(&read)), start) {
            assert_eq!(*sent.payload, sent.update.payload(), "{}", sent.key);
            sent_keys.push(sent.key.clone());
            coalescer.push(Change::Teams {
                computer: ComputerId::local(),
                key: sent.key,
                update: sent.update,
                payload: sent.payload,
            });
        }
    }
    assert!(sent_keys.contains(&team_key("team", TEAM)), "{sent_keys:?}");
    let batch = coalescer.take();
    let team = batch
        .teams
        .iter()
        .find(|out| out.key == team_key("team", TEAM))
        .unwrap();
    assert_eq!(team.kind, "team");
    assert_eq!(
        team.payload,
        TeamsUpdate::Team(std::sync::Arc::new(view::team(&fixture("status")))).payload()
    );
    while let Some(read) = model.next(start + POLL) {
        assert!(
            model
                .apply_sent(&read, Ok(replies(&read)), start + POLL)
                .is_empty(),
            "{read:?}"
        );
    }
}

#[test]
fn a_failure_keeps_a_pending_view_in_the_batch() {
    let mut coalescer = Coalescer::new();
    let team = view::team(&fixture("status"));
    let key = team_key("team", TEAM);
    let change = |update| Change::teams(ComputerId::local(), key.clone(), update);
    coalescer.push(change(TeamsUpdate::Team(std::sync::Arc::new(team))));
    coalescer.push(change(TeamsUpdate::Failed {
        code: "timed_out".into(),
        message: "late".into(),
    }));
    let batch = coalescer.take();
    assert_eq!(
        (batch.teams.len(), batch.teams[0].kind.as_str()),
        (1, "team")
    );
    coalescer.push(change(TeamsUpdate::Failed {
        code: "timed_out".into(),
        message: "late".into(),
    }));
    let batch = coalescer.take();
    assert_eq!(
        batch.teams[0].payload,
        json!({"code": "timed_out", "message": "late"})
    );
    assert!(coalescer.take().is_empty());
}

/// Task #58: status carries each phase's mode and the free phase; the list
/// names the free phases; an older hgs sends neither.
#[test]
fn task_58_each_phase_carries_its_mode() {
    let status = json!({"id":"t1","name":"Checkout","status":"running","lead":"lead","members":[{"id":"lead"}],
        "plan_mode":"standard","phase_modes":{"1":"standard","2":"free"},
        "free_phase":{"phase":2,"state":"idle","running":[]},
        "phase_summary":[{"n":1,"title":"Build","state":"running"},{"n":2,"title":"Support","state":"running"},{"n":3,"state":"draft"}]});
    let view = view::team(&status);
    assert!(view.phase_modes_known);
    assert_eq!(view.free_phase, Some(2));
    let modes: Vec<(u64, &str)> = view.phases.iter().map(|phase| (phase.n, phase.mode.as_str())).collect();
    assert_eq!(modes, [(1, "standard"), (2, "free"), (3, "standard")], "a phase not named reads the team's mode");
    let older = view::team(&json!({"id":"t1","name":"Checkout","status":"running","lead":"lead","plan_mode":"free",
        "phase_summary":[{"n":1,"state":"running"}]}));
    assert!(!older.phase_modes_known);
    assert_eq!((older.phases[0].mode.as_str(), older.free_phase), ("", None));
    let list = view::list(&json!({"items":[
        {"id":"a","name":"A","status":"running","lead":"lead","members":[],"phase_modes":{"1":"standard","3":"free"}},
        {"id":"b","name":"B","status":"running","lead":"lead","members":[]}]}));
    assert_eq!((list.teams[0].free_phases.clone(), list.teams[1].free_phases.clone()), (vec![3], vec![]));
}

/// Task #60: a follow-up's origin reaches the task view and the graph node,
/// never as a dependency edge; an older record has none.
#[test]
fn task_60_a_follow_up_carries_its_origin() {
    let status = json!({"id":"t1","name":"Checkout","status":"running","lead":"lead","members":[{"id":"lead"}],
        "tasks":[{"id":1,"title":"API","status":"accepted","assignee":"lead"},
                 {"id":2,"title":"Fix","status":"pending","assignee":"lead","origin":1},
                 {"id":3,"title":"Gone origin","status":"pending","assignee":"lead","origin":9}],
        "layers":{"1":0,"2":0,"3":0}});
    let view = view::team(&status);
    let origins: Vec<Option<u64>> = view.tasks.iter().map(|task| task.origin).collect();
    assert_eq!(origins, [None, Some(1), Some(9)]);
    let graph = view::graph(&status, &view);
    assert_eq!(graph.nodes.iter().map(|node| node.origin).collect::<Vec<_>>(), [None, Some(1), None], "a task the graph lacks is no link");
    assert!(graph.edges.is_empty(), "an origin is not a dependency edge");
}

/// Task 197: a card's time line ("after #3 ~45 min", "attempt 2, idle 12 of
/// ~30 min") needs the estimate's source and spent minutes, the running
/// attempt's start and the worked seconds, what the task waits on and whether
/// its attempt began; an older hgs names none of them, and none reaches the
/// wire then.
#[test]
fn task_197_a_card_s_time_and_wait_reach_the_task_view() {
    let status = json!({"id":"t1","name":"Checkout","status":"running","lead":"lead","members":[{"id":"lead"}],
        "tasks":[
            {"id":1,"title":"API","status":"in_progress","assignee":"lead","estimate_minutes":30,"estimate_source":"role",
             "estimate_spent_minutes":12.5,"timing":{"worked_s":600,"running_since":1700000000.0},
             "attempts":[{"n":1,"state":"delivered"},{"n":2,"state":"delivered","started_at":1700000000.0}]},
            {"id":2,"title":"UI","status":"pending","assignee":"lead","estimate_minutes":45,"estimate_source":"lead",
             "waits":{"for":"deps","on":[1,"#4"],"unblock":"finish #1"}},
            {"id":3,"title":"Old","status":"pending","assignee":"lead"}]});
    let view = view::team(&status);
    let (api, ui, old) = (&view.tasks[0], &view.tasks[1], &view.tasks[2]);
    assert_eq!((api.estimate_source.as_str(), api.estimate_spent, api.running_since, api.worked_s, api.attempt_begun),
               ("role", Some(12.5), 1_700_000_000.0, 600.0, true));
    assert_eq!((ui.wait_for.as_str(), ui.wait_on.clone(), ui.waits.as_str()), ("deps", vec!["1".to_owned(), "#4".to_owned()], "finish #1"));
    assert!(!ui.attempt_begun);
    let wire = serde_json::to_value(old).unwrap();
    for key in ["estimate_source", "estimate_spent", "running_since", "worked_s", "wait_for", "wait_on", "attempt_begun"] {
        assert!(wire.get(key).is_none(), "an older hgs adds no {key}");
    }
}

/// Task 92: the Time view's lanes from this team's real replies, as Qt's
/// `TeamProfile::Model::build` lays them out: a lane per profile member in
/// its order (then the lead when it has time), each member's own spans in its
/// lane with build and merge among them, every wait under the lane of the
/// member its task was given, idle time never over the member's own work,
/// and the critical paths from the profile.
#[test]
fn the_time_view_lays_out_qt_s_lanes_from_the_team_s_replies() {
    use super::timeline::{self, MAX_SPANS};
    let status = fixture("status");
    let profile = fixture("profile");
    let spans = fixture("spans");
    let timelines = spans["tasks"].as_array().unwrap().clone();
    let tests = vec![
        json!({"id": "r1", "suite": "client-core", "status": "passed", "task": 1,
               "at": profile["summary"]["from"].as_f64().unwrap() + 60.0, "duration_ms": 90_000}),
        json!({"id": "r2", "suite": "old", "status": "failed", "task": 2, "at": 10.0, "duration_ms": 1000}),
    ];
    let now = profile["summary"]["to"].as_f64().unwrap() + 600.0;
    let view = timeline::build(
        TEAM,
        Some(2),
        &status,
        &profile,
        &timelines,
        &tests,
        false,
        now,
    );
    assert_eq!(view.phase, Some(2));
    // The same table tray/tests/test_teamsui_profile.cpp asserts for Qt's
    // Model::build on these replies: each lane's own kinds and its waits.
    let counts = |spans: &[timeline::TimelineSpan]| {
        let mut kinds = std::collections::BTreeMap::<&str, usize>::new();
        for span in spans {
            *kinds.entry(span.kind.as_str()).or_default() += 1;
        }
        kinds
            .iter()
            .map(|(kind, n)| format!("{kind}:{n}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let table: Vec<String> = view
        .lanes
        .iter()
        .map(|lane| {
            format!(
                "{}|{}|{}",
                lane.member,
                counts(&lane.spans),
                counts(&lane.waits)
            )
        })
        .collect();
    assert_eq!(
        table,
        [
            "core|build:6,question:1,work:5|delivery:3,deps:2,lead:2,member:3,start_delay:3",
            "hgs||deps:1",
            "ui1|build:13,work:5|delivery:5,deps:2,lead:5,member:5,start_delay:5",
            "ui2|permission:2,question:8,work:11|delivery:3,deps:3,lead:1,member:6,start_delay:3",
            "merger||deps:4",
            "qa|work:1|delivery:1,deps:2,member:1,start_delay:1",
        ]
    );
    let members: Vec<&str> = profile["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["member"].as_str().unwrap())
        .collect();
    let lanes: Vec<&str> = view.lanes.iter().map(|lane| lane.member.as_str()).collect();
    assert_eq!(
        &lanes[..members.len()],
        &members[..],
        "the profile's members, in its order"
    );
    assert!(lanes.len() <= members.len() + 1);
    let assignee = |task: u64| {
        status["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == task)
            .map(|row| row["assignee"].as_str().unwrap().to_owned())
            .unwrap_or_default()
    };
    let busy = [
        "work",
        "rework",
        "stage",
        "question",
        "permission",
        "background",
    ];
    // Every span of the replies is drawn once, in Qt's lane.
    let (mut own, mut waited, mut builds) = (0, 0, 0);
    for task in &timelines {
        let id = task["task"].as_u64().unwrap();
        for raw in task["spans"].as_array().unwrap() {
            let Some(from) = raw["from"].as_f64() else {
                continue;
            };
            if raw["to"].as_f64().is_some_and(|to| to <= from) {
                continue;
            }
            let kind = raw["kind"].as_str().unwrap();
            if busy.contains(&kind) {
                own += 1;
                let who = raw["member"]
                    .as_str()
                    .filter(|m| !m.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| assignee(id));
                if let Some(lane) = view.lanes.iter().find(|lane| lane.member == who) {
                    assert!(
                        lane.spans
                            .iter()
                            .any(|span| span.task == id && span.kind == kind && span.from == from),
                        "{kind} of #{id} in {who}"
                    );
                }
            } else {
                waited += 1;
                if let Some(lane) = view.lanes.iter().find(|lane| lane.member == assignee(id)) {
                    assert!(
                        lane.waits
                            .iter()
                            .any(|span| span.task == id && span.kind == kind && span.from == from),
                        "{kind} of #{id}"
                    );
                    assert!(
                        !lane.spans.iter().any(|span| span.kind == kind),
                        "a wait is never a lane's own span"
                    );
                }
            }
        }
        builds += task["overlay"].as_array().map_or(0, |overlay| {
            overlay
                .iter()
                .filter(|b| b["from"].as_f64().is_some())
                .count()
        });
    }
    let drawn_own: usize = view
        .lanes
        .iter()
        .map(|lane| {
            lane.spans
                .iter()
                .filter(|span| span.kind != "build")
                .count()
        })
        .sum();
    let drawn_waits: usize = view.lanes.iter().map(|lane| lane.waits.len()).sum();
    let drawn_builds: usize = view
        .lanes
        .iter()
        .map(|lane| {
            lane.spans
                .iter()
                .filter(|span| span.kind == "build")
                .count()
        })
        .sum();
    assert!(own > 0 && waited > 0 && builds > 0);
    assert!(drawn_own <= own && drawn_waits <= waited && drawn_builds <= builds);
    let kinds: std::collections::BTreeSet<&str> = view
        .lanes
        .iter()
        .flat_map(|lane| lane.waits.iter().map(|span| span.kind.as_str()))
        .collect();
    for kind in ["member", "deps", "delivery", "start_delay", "lead"] {
        assert!(kinds.contains(kind), "the {kind} waits are drawn");
    }
    for lane in &view.lanes {
        assert!(
            lane.spans
                .windows(2)
                .all(|pair| pair[0].from <= pair[1].from),
            "a lane's spans are in time order"
        );
        for idle in &lane.idle {
            assert!(idle[0] < idle[1]);
            assert!(
                !lane
                    .spans
                    .iter()
                    .filter(|span| busy.contains(&span.kind.as_str()))
                    .any(|span| span.from < idle[1] - 1e-6 && span.to > idle[0] + 1e-6),
                "{} is idle while it works",
                lane.member
            );
        }
    }
    // The window covers every span; the critical paths come from the profile.
    assert!(view.from > 0.0 && view.to > view.from);
    assert!(view
        .lanes
        .iter()
        .flat_map(|lane| lane.spans.iter().chain(&lane.waits))
        .all(|span| span.from >= view.from && span.to <= view.to));
    assert_eq!(
        view.real_s,
        profile["critical_path"]["real"]["s"].as_u64().unwrap()
    );
    assert_eq!(
        view.real.iter().map(|span| span.task).collect::<Vec<_>>(),
        [1]
    );
    assert!(
        view.planned
            .windows(2)
            .all(|pair| pair[0].to == pair[1].from),
        "the planned path runs end to end"
    );
    assert!(view
        .planned
        .iter()
        .all(|span| [4, 6, 7, 12, 13, 14].contains(&span.task)));
    // Only the window's test runs, named by suite; titles only for drawn tasks.
    assert_eq!(view.tests.len(), 1);
    assert_eq!(
        (
            view.tests[0].member.as_str(),
            view.tests[0].stage_kind.as_str(),
            view.tests[0].record.as_str()
        ),
        ("client-core", "passed", "r1")
    );
    assert!(!view.tests_more);
    assert!(
        view.titles.contains_key(&1)
            && view
                .titles
                .values()
                .all(|title| title.chars().count() <= timeline::TITLE_CHARS)
    );
    assert!(!view.estimates.is_empty());
    assert_eq!(view.tasks, timelines.len() as u64);
    assert!(!view.truncated);

    // Bounded: a flood of spans keeps MAX_SPANS and says so.
    let flood: Vec<Value> = (0..3).map(|n| json!({"task": n + 1, "spans": (0..MAX_SPANS / 2).map(|i| json!({"kind": "work", "member": "ui1",
        "from": 1000.0 + i as f64, "to": 1000.5 + i as f64})).collect::<Vec<_>>()})).collect();
    let flooded = timeline::build(
        TEAM,
        None,
        &status,
        &json!({"members": [{"member": "ui1"}]}),
        &flood,
        &[],
        false,
        0.0,
    );
    assert!(flooded.truncated);
    assert_eq!(flooded.lanes[0].spans.len(), MAX_SPANS);
}

/// Task 118: time that waited for a person's answer is a wait under its
/// task's lane, never the member's own busy time, so it counts as neither work
/// nor busy and the lane's idle outline is not drawn over it.
#[test]
fn a_wait_for_a_person_is_drawn_as_a_wait_and_not_as_the_member_s_own_time() {
    use super::timeline;
    let status = fixture("status");
    // Task 1 is ui2's.
    let timelines = vec![json!({"task": 1, "spans": [
        {"kind": "work", "member": "ui2", "from": 1000.0, "to": 1100.0},
        {"kind": "person", "reason": "permission", "member": "ui2", "from": 1100.0, "to": 1400.0},
        {"kind": "work", "member": "ui2", "from": 1400.0, "to": 1500.0},
    ]})];
    let view = timeline::build(
        TEAM,
        None,
        &status,
        &json!({"members": [{"member": "ui2"}]}),
        &timelines,
        &[],
        false,
        1600.0,
    );
    let lane = view.lanes.iter().find(|lane| lane.member == "ui2").unwrap();
    assert_eq!(
        lane.spans.iter().map(|span| span.kind.as_str()).collect::<Vec<_>>(),
        ["work", "work"]
    );
    assert_eq!(
        lane.waits.iter().map(|span| (span.kind.as_str(), span.from, span.to)).collect::<Vec<_>>(),
        [("person", 1100.0, 1400.0)]
    );
}

/// Task 92: the Time view's reads name the scope and their bounds.
#[test]
fn the_time_view_reads_a_scope_in_bounded_pages() {
    let profile = TeamRead::ScopedProfile {
        team: TEAM.into(),
        phase: Some(3),
    };
    assert_eq!(profile.op(), "profile");
    assert_eq!(
        profile.request(),
        json!({"team": TEAM, "limit": 20, "phase": 3})
    );
    assert_eq!(
        TeamRead::ScopedProfile {
            team: TEAM.into(),
            phase: None
        }
        .request(),
        json!({"team": TEAM, "limit": 20})
    );
    let spans = TeamRead::ScopedSpans {
        team: TEAM.into(),
        phase: Some(3),
        after: 50,
    };
    assert_eq!(spans.op(), "spans");
    assert_eq!(
        spans.request(),
        json!({"team": TEAM, "limit": 50, "phase": 3, "after": 50})
    );
    assert_eq!(
        TeamRead::ScopedSpans {
            team: TEAM.into(),
            phase: None,
            after: 0
        }
        .request(),
        json!({"team": TEAM, "limit": 50})
    );
    let tests = TeamRead::TestWindow {
        team: TEAM.into(),
        from: Some(100.0),
        before: Some(201.0),
    };
    assert_eq!(
        tests.request(),
        json!({"team": TEAM, "action": "results", "limit": 100, "from": 100.0, "before": 201.0})
    );
    let call = invocation(&ComputerId::from("studio"), &tests).unwrap();
    assert_eq!(call.args, ["@studio", "team", "test", "--json"]);
    assert_eq!(
        invocation(&ComputerId::local(), &spans).unwrap().args,
        ["team", "spans", "--json"]
    );
    // A spans reply without its tasks is refused before it is projected.
    assert!(decode(
        &spans,
        Exit::Code {
            code: 0,
            stdout: br#"{"team":"x"}"#.to_vec(),
            stderr: String::new()
        }
    )
    .is_err());
}

/// Task 160: a parked task carries hgs status's words; any other task omits
/// the field, so its wire shape is unchanged.
#[test]
fn task_160_a_parked_task_says_until_when() {
    let status = json!({"id":"t1","name":"Checkout","status":"running","lead":"lead",
        "members":[{"id":"lead","role":"Lead"},{"id":"dev","role":"Backend"}],
        "tasks":[{"id":1,"title":"API","status":"pending","assignee":"dev",
                  "parked":"parked until hgs version 1.58.0 installed, ETA 14:30 (in 25 min)"},
                 {"id":2,"title":"Docs","status":"pending","assignee":"dev"}]});
    let view = super::view::team(&status);
    assert_eq!(view.tasks[0].parked, "parked until hgs version 1.58.0 installed, ETA 14:30 (in 25 min)");
    assert_eq!(view.tasks[1].parked, "");
    let wire = serde_json::to_value(&view.tasks[1]).unwrap();
    assert!(wire.get("parked").is_none(), "{wire}");
    assert_eq!(serde_json::to_value(&view.tasks[0]).unwrap()["parked"], view.tasks[0].parked.as_str());
}

/// Task 209: a member card shows its native context reading, and a member
/// whose context is being compacted reads `compacting`.
#[test]
fn task_209_a_member_card_shows_its_context_and_compacting_state() {
    let status = json!({"id":"t1","name":"Checkout","status":"running","lead":"lead",
        "members":[
            {"id":"lead","role":"Lead"},
            {"id":"dev","role":"Backend","agent":"claude","state":"active",
             "context":{"used":960000,"limit":1000000,"percent":96,"at":1000.0}},
            {"id":"qa","role":"Tester","agent":"dsh","state":"compacting",
             "context":{"used":970000,"limit":1000000,"percent":97,"at":1000.5},
             "compacting":{"action":"compact","started_at":1000.0,"task":4,"reason":"threshold"}}
        ],
        "tasks":[]});
    let view = super::view::team(&status);
    let dev = view.members.iter().find(|member| member.id == "dev").unwrap();
    let context = dev.context.as_ref().expect("the reading is on the card");
    assert_eq!((context.used, context.limit, context.percent), (960000, 1000000, 96));
    assert!(!dev.compacting);
    let qa = view.members.iter().find(|member| member.id == "qa").unwrap();
    assert!(qa.compacting);
    assert_eq!(qa.state, "compacting");
    assert_eq!(qa.context.as_ref().unwrap().percent, 97);
    // A member with no meter shows none, and its wire shape is unchanged.
    let lead = view.members.iter().find(|member| member.id == "lead").unwrap();
    assert!(lead.context.is_none() && !lead.compacting);
    assert!(serde_json::to_value(lead).unwrap().get("context").is_none());
}

/// Task 196: the phase header's details from the phase records (Qt
/// TeamWorkspace phase header): the lead's note, the send-back and whether it
/// was reworked, who proposed a draft, "accepted by Auto", the queue's words,
/// the lead's changes and the pause; a draft task carries what the plan's task
/// editor shows, a handed-out one does not.
#[test]
fn task_196_the_phase_header_reads_the_phase_records() {
    let status = json!({"id":"t1","name":"Checkout","status":"running","lead":"lead","members":[{"id":"lead"}],
        "phase_summary":[
            {"n":1,"state":"running"},
            {"n":2,"state":"queued","acceptance":"owner","pause":{"by":"human","owner":true,"reason":"after the release"}},
            {"n":3,"state":"rejected"},
            {"n":4,"state":"draft","auto":true,"start_ahead":true}],
        "phases":[
            {"n":2,"state":"queued","decided_by":"Auto","note":"Ship first","waiting":{"text":"Starts after phase 1"},"changes":[{"at":1},{"at":2}]},
            {"n":3,"state":"rejected","rejection":{"comment":"Split it","reworked_at":0},"added_by":"Lead","added_at":1700000000.0},
            {"n":4,"state":"draft"}],
        "tasks":[
            {"id":1,"title":"API","status":"running","assignee":"lead","phase":1,"description":"secret","criteria":["a"],"attempts":[{"id":"x"}]},
            {"id":2,"title":"Docs","status":"draft","assignee":"lead","phase":4,"description":"Write docs","criteria":["Readable"],"attempts":[]}]});
    let view = view::team(&status);
    let phase = |n: u64| view.phases.iter().find(|phase| phase.n == n).unwrap().clone();
    let queued = phase(2);
    assert_eq!((queued.note.as_str(), queued.waiting.as_str(), queued.changes), ("Ship first", "Starts after phase 1", 2));
    assert!(queued.accepted_by_auto && queued.owner_accepts && queued.paused && queued.pause_owner);
    assert_eq!((queued.pause_by.as_str(), queued.pause_reason.as_str()), ("human", "after the release"));
    let rejected = phase(3);
    assert_eq!((rejected.rejection.as_str(), rejected.reworked, rejected.added_by.as_str()), ("Split it", false, "Lead"));
    assert_eq!(rejected.added_at, 1700000000.0);
    assert!(phase(4).start_ahead && !phase(4).accepted_by_auto);
    assert!(phase(1).note.is_empty() && !phase(1).accepted_by_auto);
    let wire = serde_json::to_value(phase(1)).unwrap();
    assert!(wire.get("note").is_none() && wire.get("changes").is_none(), "empty details stay off the wire: {wire}");
    let task = |id: u64| view.tasks.iter().find(|task| task.id == id).unwrap().clone();
    assert_eq!((task(2).description.as_str(), task(2).criteria.clone()), ("Write docs", vec!["Readable".to_owned()]));
    assert!(task(1).description.is_empty() && task(1).criteria.is_empty(), "a handed-out task's text is not sent");
}

/// Task 199: the member card's details: who it reports to and whether it is a
/// sub-lead, a drain, a usage limit, the account its session runs on, its
/// services (bounded) and its budget; an older hgs sends none of them.
#[test]
fn task_199_a_member_carries_its_card_details() {
    let services: Vec<Value> = (0..20).map(|n| json!({"name": format!("mcp-{n}"), "since": 1.0, "command": "npx server"})).collect();
    let status = json!({"id":"t1","name":"Checkout","status":"running","lead":"lead","members":[
        {"id":"lead","role":"Lead","agent":"claude"},
        {"id":"arch","role":"Architect","agent":"claude","sub":true,"session_account":"native-claude"},
        {"id":"api","role":"Backend","agent":"codex","parent":"arch","draining":true,"limit_resume":true,
         "limited":{"at":10.0,"resets_at":20.0,"detail":"5-hour limit"},"services":services,
         "budget":{"label":"build","state":"amber","minutes":15,"red_minutes":30,"elapsed_minutes":18}}]});
    let view = view::team(&status);
    let member = |id: &str| view.members.iter().find(|member| member.id == id).unwrap().clone();
    let arch = member("arch");
    assert!(arch.sub && arch.parent.is_empty());
    assert_eq!(arch.session_account.as_deref(), Some("native-claude"));
    let api = member("api");
    assert_eq!((api.parent.as_str(), api.draining, api.limit_resume), ("arch", true, true));
    assert_eq!(api.limited.as_ref().map(|limit| (limit.resets_at, limit.detail.as_str())), Some((20.0, "5-hour limit")));
    assert_eq!(api.services.len(), 12, "services are bounded");
    assert_eq!(api.budget.as_ref().map(|budget| (budget.state.as_str(), budget.elapsed_minutes)), Some(("amber", 18)));
    let lead = member("lead");
    assert!(lead.session_account.is_none() && lead.limited.is_none() && lead.services.is_empty());
    let wire = serde_json::to_value(&lead).unwrap();
    assert!(wire.get("sub").is_none() && wire.get("services").is_none() && wire.get("session_account").is_none(), "{wire}");
}

/// Task 199: the Builds view of `hgs team builds` (broker.rs builds_view):
/// each shared folder, its build folders and what runs, waits and finished,
/// bounded; the read sends `{team}` to `team builds`.
#[test]
fn task_199_builds_are_read_and_bounded() {
    use super::read::{invocation, TeamRead};
    let read = TeamRead::Builds { team: "t1".into() };
    let call = invocation(&ComputerId::local(), &read).unwrap();
    assert_eq!(call.args, ["team", "builds", "--json"]);
    assert_eq!(serde_json::from_slice::<Value>(call.stdin.as_deref().unwrap()).unwrap(), json!({"team": "t1"}));
    let timeline: Vec<Value> = (0..30).map(|n| json!({"member":"api","template":"cargo-build","at":n as f64,"hit":n % 2 == 0,"by":"ui","ms":1200,"exit":0})).collect();
    let value = json!({"team":"t1","enabled":true,"folders":[{"folder":"D:/work/shared","members":["api","ui"],"build_folders":[
        {"name":".","usual_ms":90000.0,"hit_rate":0.5,
         "running":[{"member":"api","template":"cargo-build","started":100.0,"seconds":30.0,"usual_ms":90000.0,"snapshot":true}],
         "queue":[{"member":"ui","template":"cargo-build","since":110.0,"seconds":20.0,"coalesced_with":"api","waiting_for":["qa"]}],
         "timeline":timeline}],"changes":[],"overlaps":[]}],"members":{}});
    let view = view::builds("t1", &value);
    assert!(view.enabled);
    let group = &view.folders[0].build_folders[0];
    assert_eq!((view.folders[0].members.len(), group.name.as_str(), group.hit_rate), (2, ".", Some(0.5)));
    assert_eq!((group.running[0].at, group.running[0].snapshot), (100.0, true));
    assert_eq!((group.queue[0].coalesced_with.as_str(), group.queue[0].waiting_for.clone()), ("api", vec!["qa".to_owned()]));
    assert_eq!(group.recent.len(), 20, "recent builds are bounded");
    assert!(group.recent[0].hit && group.recent[0].by == "ui");
    let empty = view::builds("t1", &json!({"folders":[]}));
    assert!(!empty.enabled && empty.folders.is_empty());
}

const OTHER: &str = "2b863c49-6255-4580-8cd1-82dd023a556a";

fn follow(team: &str) -> TeamsFollow {
    TeamsFollow { team: Some(team.into()), ..follow_team() }
}

/// Task #14 (team ede724c4, owner 2026-11-10): the opened tab's status is the
/// first read, ahead of the list, Waiting and asked-for reads.
#[test]
fn the_watched_teams_status_is_read_first() {
    let start = Instant::now();
    let mut model = TeamsModel::default();
    // The Teams page has run for a while: the list and Waiting are due.
    model.follow(TeamsFollow { team: None, ..follow_team() }, start);
    assert_eq!(model.next(start), Some(TeamRead::List));
    model.apply(&TeamRead::List, Ok(replies(&TeamRead::List)), start);
    let later = start + POLL * 2;
    model.follow(follow_team(), later);
    assert!(model.request_profile(), "an asked-for read waits too");
    assert_eq!(model.next(later), Some(TeamRead::Status { team: TEAM.into() }), "the tab's status goes first");
    let rest: Vec<TeamRead> = std::iter::from_fn(|| model.next(later)).collect();
    assert_eq!(rest.first(), Some(&TeamRead::Profile { team: TEAM.into() }), "{rest:?}");
    assert!(rest.contains(&TeamRead::List) && rest.contains(&TeamRead::Waiting { before: None }), "{rest:?}");
    assert!(!rest.iter().any(|read| matches!(read, TeamRead::Status { .. })), "one status read: {rest:?}");
}

/// Task #14: a reply about the team no longer watched does not move the newly
/// watched team's slots: its status is still read at once (it waited a whole
/// interval - 60 s with the stream's heads - behind that reply).
#[test]
fn a_stale_reply_does_not_delay_the_new_teams_status() {
    let start = Instant::now();
    let mut model = TeamsModel::default();
    model.follow(follow(OTHER), start);
    let stale = model.next(start).unwrap();
    assert_eq!(stale, TeamRead::Status { team: OTHER.into() });
    // The owner opens another team while that read runs.
    model.follow(follow(TEAM), start);
    let out = model.apply(&stale, Ok(replies(&stale)), start);
    assert!(out.is_empty(), "nothing about the team no longer watched: {out:?}");
    assert_eq!(model.next(start), Some(TeamRead::Status { team: TEAM.into() }));
    // Beside a read in flight the poller asks for the status alone.
    let mut model = TeamsModel::default();
    model.follow(follow(OTHER), start);
    let inbox = std::iter::from_fn(|| model.next(start)).find(|read| matches!(read, TeamRead::Inbox { .. }));
    assert!(inbox.is_some());
    model.follow(follow(TEAM), start);
    assert_eq!(model.next_status(start), Some(TeamRead::Status { team: TEAM.into() }));
    assert_eq!(model.next_status(start), None, "once");
}

/// Task #14: a tab opened again shows the views held for its team at once,
/// the fresh read sends nothing when it found the same, and opening it costs
/// the same reads as before: no extra hgs process.
#[test]
fn a_reopened_tab_shows_its_held_team_at_once() {
    let start = Instant::now();
    let mut model = TeamsModel::default();
    model.follow(follow(TEAM), start);
    let first = poll_once(&mut model, start, replies);
    assert!(first.iter().any(|(key, _)| *key == team_key("team", TEAM)));
    // Another team, then this one again.
    model.follow(follow(OTHER), start);
    poll_once(&mut model, start, replies);
    let shown = model.follow(follow(TEAM), start);
    let keys: Vec<&str> = shown.iter().map(|(key, _)| key.as_str()).collect();
    assert!(keys.contains(&team_key("team", TEAM).as_str()), "the held status shows at once: {keys:?}");
    assert!(keys.contains(&team_key("inbox", TEAM).as_str()), "{keys:?}");
    assert!(keys.iter().any(|key| key.starts_with(&format!("team:{OTHER}"))), "the other team's views end: {keys:?}");
    // The fresh reads run as for a first open; an unchanged reply sends nothing.
    let mut reads = Vec::new();
    let mut out = Vec::new();
    while let Some(read) = model.next(start) {
        out.extend(model.apply(&read, Ok(replies(&read)), start));
        reads.push(read);
    }
    let team_reads = reads.iter().filter(|read| read.team() == Some(TEAM)).count();
    assert_eq!(team_reads, 3, "status, inbox and feed once each: {reads:?}");
    assert!(!out.iter().any(|(key, _)| *key == team_key("team", TEAM) || *key == team_key("inbox", TEAM)), "{out:?}");
    // Nothing more until the interval.
    assert_eq!(model.next(start + POLL - Duration::from_millis(1)), None);
}

/// Task #14: the poller runs the opened tab's status beside a read of the
/// team no longer watched (a slow SSH read) instead of waiting behind it.
#[test]
fn the_poller_never_holds_a_tab_behind_another_teams_read() {
    use std::sync::{Arc, Mutex};
    let started = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = Arc::clone(&started);
    let (tx, rx) = mpsc::channel();
    let poller = super::TeamsPoller::spawn_with(
        ComputerId::local(),
        Box::new(move |read: &TeamRead| {
            log.lock().unwrap().push(format!("{read:?}"));
            // Every read of the first team after its status takes 5 s.
            if read.team() == Some(OTHER) && !matches!(read, TeamRead::Status { .. }) {
                std::thread::sleep(Duration::from_secs(5));
            }
            Ok(replies(read))
        }),
        Box::new(move |change| {
            let _ = tx.send(change);
        }),
    )
    .unwrap();
    poller.follow(TeamsFollow { list: false, waiting: false, team: Some(OTHER.into()), catalog: false });
    let wait_key = |key: String, within: Duration| -> Option<Duration> {
        let asked = Instant::now();
        while asked.elapsed() < within {
            if let Ok(Change::Teams { key: got, .. }) = rx.recv_timeout(Duration::from_millis(50)) {
                if got == key {
                    return Some(asked.elapsed());
                }
            }
        }
        None
    };
    assert!(wait_key(team_key("team", OTHER), Duration::from_secs(10)).is_some());
    // Its slow inbox read is in flight now; the owner opens the other tab.
    let deadline = Instant::now() + Duration::from_secs(2);
    while !started.lock().unwrap().iter().any(|read| read.starts_with("Inbox")) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    poller.follow(TeamsFollow { list: false, waiting: false, team: Some(TEAM.into()), catalog: false });
    let took = wait_key(team_key("team", TEAM), Duration::from_secs(10)).expect("the tab's team");
    assert!(took < Duration::from_secs(2), "the tab waited {took:?} behind the other team's read");
    drop(poller);
}

/// Task #16 of team c536f0e5 (P2.5): the frames of the stream's `team.views`
/// topic carrying `rows`, through the client's own frame path.
fn view_frames(state: &mut crate::CatalogState, rows: &[(String, Value)], seq: u64) -> Vec<Change> {
    let items: Vec<Value> = rows.iter().map(|(key, row)| json!({"key": key, "row": row})).collect();
    let frame = serde_json::from_value(json!({"v":1,"seq":seq,"t":"snapshot","topic":"team.views","page":0,"last":true,"cursor":seq,"items":items})).unwrap();
    state.apply(&ComputerId::local(), &frame)
}

fn hello_with_views(state: &mut crate::CatalogState) {
    let hello = serde_json::from_value(json!({"v":1,"seq":1,"t":"hello","host":"h","hgs_version":"1","stream":"s",
        "heartbeat_ms":10000,"max_frame_bytes":1048576,"topics":["catalog","team","team.views"],"limits":{"activity_subscriptions":16,"page_bytes":524288,
        "queue_bytes":4194304,"activity_tail":100,"activity_read":200,"control_line_bytes":65536}})).unwrap();
    state.apply(&ComputerId::local(), &hello);
    assert!(state.team_views_offered());
}

/// The rows a `team.views` server sends for the followed team.
fn view_rows() -> Vec<(String, Value)> {
    vec![
        ("list".into(), replies(&TeamRead::List)),
        ("waiting".into(), replies(&TeamRead::Waiting { before: None })),
        (format!("status:{TEAM}"), replies(&status())),
        (format!("inbox:{TEAM}"), replies(&TeamRead::Inbox { team: TEAM.into() })),
        (format!("feed:{TEAM}"), replies(&TeamRead::Events { team: TEAM.into(), before: None })),
    ]
}

fn streamed_views(changes: Vec<Change>) -> Option<crate::teams::ViewRows> {
    changes.into_iter().find_map(|change| match change {
        Change::TeamViews { views, .. } => views,
        _ => None,
    })
}

#[test]
fn task_16_the_same_replies_through_team_views_frames_give_the_same_updates_and_no_reads() {
    let start = Instant::now();
    let mut polled = TeamsModel::default();
    polled.follow(follow_team(), start);
    let mut expected = poll_once(&mut polled, start, replies);
    expected.sort_by(|a, b| a.0.cmp(&b.0));

    let mut state = crate::CatalogState::new();
    hello_with_views(&mut state);
    let rows = streamed_views(view_frames(&mut state, &view_rows(), 2)).expect("a complete snapshot gives the rows");
    let mut streamed = TeamsModel::default();
    streamed.follow(follow_team(), start);
    let mut got: Vec<(String, TeamsUpdate)> = streamed.views(Some(&rows), start).into_iter().map(|sent| (sent.key, sent.update)).collect();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(got, expected, "the frame path gives exactly the polled updates");
    // Nothing is read with `hgs team` any more, now or later.
    assert_eq!(streamed.next(start), None);
    assert_eq!(streamed.next(start + POLL * 100), None);
    // The same rows again change nothing; a changed inbox sends only it.
    assert!(streamed.views(Some(&rows), start).is_empty());
    let mut inbox = replies(&TeamRead::Inbox { team: TEAM.into() });
    inbox["items"] = json!([]);
    let delta = serde_json::from_value(json!({"v":1,"seq":3,"t":"delta","topic":"team.views","cursor":3,
        "upsert":[{"key":format!("inbox:{TEAM}"),"row":inbox}],"remove":[]})).unwrap();
    let changed = streamed_views(state.apply(&ComputerId::local(), &delta)).unwrap();
    let keys: Vec<String> = streamed.views(Some(&changed), start).into_iter().map(|sent| sent.key).collect();
    assert_eq!(keys, [team_key("inbox", TEAM)]);
}

#[test]
fn task_16_a_lost_stream_or_an_error_row_returns_those_reads_to_the_poller() {
    let start = Instant::now();
    let mut state = crate::CatalogState::new();
    hello_with_views(&mut state);
    let mut rows = view_rows();
    // The status was too large for a frame: read with its command.
    rows[2].1 = json!({"error":{"code":"too_large","bytes":600000,"message":"read it with hgs team status"}});
    let rows = streamed_views(view_frames(&mut state, &rows, 2)).unwrap();
    let mut model = TeamsModel::default();
    model.follow(follow_team(), start);
    model.views(Some(&rows), start);
    assert_eq!(model.next(start), Some(status()), "only the status is read");
    assert_eq!(model.next(start), None);
    // Another team is opened: its views are read until its rows arrive.
    model.follow(TeamsFollow { team: Some("other".into()), ..follow_team() }, start);
    let mut reads = Vec::new();
    while let Some(read) = model.next(start) {
        reads.push(read);
    }
    assert_eq!(reads.len(), 3, "status, inbox and feed of the new team: {reads:?}");
    // The stream drops (a gap): every followed read is polled again.
    let gap = serde_json::from_value(json!({"v":1,"seq":3,"t":"gap","topic":"team.views","reason":"overflow"})).unwrap();
    let changes = state.apply(&ComputerId::local(), &gap);
    assert!(changes.iter().any(|change| matches!(change, Change::TeamViews { views: None, .. })));
    let mut model = TeamsModel::default();
    model.follow(follow_team(), start);
    model.views(Some(&view_rows().into_iter().collect::<Vec<_>>()), start);
    assert_eq!(model.next(start), None);
    model.views(None, start);
    let mut reads = Vec::new();
    while let Some(read) = model.next(start) {
        reads.push(read);
    }
    assert_eq!(reads.len(), 5, "list, Waiting, status, inbox and feed: {reads:?}");
}

#[test]
fn task_16_a_streamed_waiting_page_keeps_the_watcher_from_reading() {
    let share = super::poller::WaitingShare::default();
    let start = Instant::now();
    share.put(&json!({"items":[]}), start);
    let later = start + Duration::from_secs(120);
    assert!(share.fresh(Duration::from_secs(35), later).is_none(), "a read reply ages");
    share.set_streamed(true);
    assert!(share.fresh(Duration::from_secs(35), later).is_some(), "an unchanged streamed view sends nothing and stays current");
    share.set_streamed(false);
    assert!(share.fresh(Duration::from_secs(35), later).is_none());
    let mut model = TeamsModel::default();
    model.follow(follow_team(), start);
    assert!(!model.waiting_streamed());
    model.views(Some(&view_rows()), start);
    assert!(model.waiting_streamed());
    model.views(None, start);
    assert!(!model.waiting_streamed());
}
